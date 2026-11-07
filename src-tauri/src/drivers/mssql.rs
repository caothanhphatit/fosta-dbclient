//! SQL Server driver built on tiberius. Tables are addressed as
//! `schema.table` inside a database, so one tree level maps to `[db].[schema].[table]`.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use futures_util::TryStreamExt;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, QueryItem, Row, ToSql};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::{
    relations::{self, TableRelations},
    types::{
        elapsed_millis, encode_hex, BrowseRequest, Cell, CellValue, ColumnDesign, ColumnMeta, QueryOutcome, ResultSet, RowChange,
        TableDesign, TableInfo, TablePage, TextRow,
    },
    Driver,
};
use crate::{
    config::ConnectionProfile,
    error::{AppError, AppResult},
    lock::lock,
    schema::{BulkSchema, SchemaColumn},
};

type TdsClient = Client<Compat<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_QUERY_ROWS: usize = 1000;
const MAX_PAGE_SIZE: u64 = 5000;
const DEFAULT_SCHEMA: &str = "dbo";
const READ_KEYWORDS: [&str; 6] = ["select", "with", "exec", "execute", "declare", "sp_"];

pub struct MssqlConnection {
    config: Config,
    /// Shared client for browsing and metadata; one request at a time.
    main: AsyncMutex<Option<TdsClient>>,
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<TdsClient>>>>,
}

impl MssqlConnection {
    pub async fn open(profile: &ConnectionProfile, password: &str) -> AppResult<(Self, String)> {
        let config = build_config(profile, password);
        let mut client = connect(&config).await?;
        let version = server_version(&mut client).await?;
        Ok((
            Self {
                config,
                main: AsyncMutex::new(Some(client)),
                sessions: Mutex::default(),
            },
            version,
        ))
    }

    /// Runs `action` on the shared client, reconnecting once if the socket died.
    async fn with_main<T, F>(&self, action: F) -> AppResult<T>
    where
        F: for<'c> FnOnce(&'c mut TdsClient) -> futures_util::future::BoxFuture<'c, AppResult<T>> + Send,
    {
        let mut guard = self.main.lock().await;
        if guard.is_none() {
            *guard = Some(connect(&self.config).await?);
        }
        let client = guard.as_mut().expect("client was just created");
        let result = action(client).await;
        if result.is_err() {
            // A failed statement is fine to keep; a dead socket is not. Dropping the
            // client means the next call reconnects, which covers both cases.
            *guard = None;
        }
        result
    }

    fn session(&self, session_id: &str) -> AppResult<Arc<AsyncMutex<TdsClient>>> {
        lock(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::new("This query tab lost its session. Close it and open a new query."))
    }
}

#[async_trait]
impl Driver for MssqlConnection {
    async fn list_databases(&self) -> AppResult<Vec<String>> {
        self.with_main(|client| {
            Box::pin(async move {
                let rows = fetch(client, "SELECT name FROM sys.databases WHERE state = 0 ORDER BY name", &[]).await?;
                Ok(rows.iter().map(|row| row.text(0)).collect())
            })
        })
        .await
    }

    async fn create_database(&self, name: &str, _first_collection: Option<&str>) -> AppResult<()> {
        let sql = format!("CREATE DATABASE {}", bracket(name));
        self.with_main(move |client| {
            Box::pin(async move {
                client.simple_query(sql).await?.into_results().await?;
                Ok(())
            })
        })
        .await
    }

    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>> {
        let sql = format!(
            "SELECT s.name, t.name, 'table', SUM(p.rows) \
             FROM {db}.sys.tables t \
             JOIN {db}.sys.schemas s ON s.schema_id = t.schema_id \
             LEFT JOIN {db}.sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1) \
             GROUP BY s.name, t.name \
             UNION ALL \
             SELECT s.name, v.name, 'view', NULL \
             FROM {db}.sys.views v JOIN {db}.sys.schemas s ON s.schema_id = v.schema_id \
             ORDER BY 1, 2",
            db = bracket(database)
        );
        self.with_main(|client| {
            Box::pin(async move {
                let rows = fetch(client, &sql, &[]).await?;
                Ok(rows
                    .iter()
                    .map(|row| TableInfo {
                        name: object_name(&row.text(0), &row.text(1)),
                        kind: row.text(2),
                        rows: row.text(3).parse().ok(),
                        comment: String::new(),
                    })
                    .collect())
            })
        })
        .await
    }

    async fn describe_table(&self, database: &str, table: &str) -> AppResult<TableDesign> {
        let (schema, name) = split_object(table);
        let db = bracket(database);
        let columns_sql = format!(
            "SELECT c.COLUMN_NAME, c.DATA_TYPE, c.CHARACTER_MAXIMUM_LENGTH, c.NUMERIC_PRECISION, c.NUMERIC_SCALE, \
                    c.IS_NULLABLE, c.COLUMN_DEFAULT, \
                    COLUMNPROPERTY(OBJECT_ID(@P3), c.COLUMN_NAME, 'IsIdentity') \
             FROM {db}.INFORMATION_SCHEMA.COLUMNS c \
             WHERE c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2 ORDER BY c.ORDINAL_POSITION"
        );
        let qualified = format!("{db}.{}.{}", bracket(&schema), bracket(&name));
        let key_sql = primary_key_sql(database);
        self.with_main(|client| {
            Box::pin(async move {
                let rows = fetch(client, &columns_sql, &[&schema, &name, &qualified]).await?;
                let keys: Vec<String> = fetch(client, &key_sql, &[&schema, &name])
                    .await?
                    .iter()
                    .map(|row| row.text(0))
                    .collect();
                let columns: Vec<ColumnDesign> = rows
                    .iter()
                    .map(|row| ColumnDesign {
                        name: row.text(0),
                        column_type: type_label(&row.text(1), row.optional(2), row.optional(3), row.optional(4)),
                        nullable: row.text(5) == "YES",
                        key: if keys.contains(&row.text(0)) { "PRI" } else { "" }.to_owned(),
                        default_value: row.optional(6),
                        extra: if row.text(7) == "1" { "identity" } else { "" }.to_owned(),
                        comment: String::new(),
                    })
                    .collect();
                let ddl = synthesize_ddl(&qualified, &columns, &keys);
                Ok(TableDesign { columns, ddl })
            })
        })
        .await
    }

    async fn browse_table(&self, request: &BrowseRequest) -> AppResult<TablePage> {
        let (schema, name) = split_object(&request.table);
        let source = format!("{}.{}.{}", bracket(&request.database), bracket(&schema), bracket(&name));
        let condition = where_clause(request.filter.as_deref());
        let ordering = match request.order_by.as_deref() {
            Some(column) => format!("ORDER BY {} {}", bracket(column), if request.descending { "DESC" } else { "ASC" }),
            None => "ORDER BY (SELECT NULL)".to_owned(),
        };
        let limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        let page_sql = format!(
            "SELECT * FROM {source}{condition} {ordering} OFFSET {} ROWS FETCH NEXT {limit} ROWS ONLY",
            request.offset
        );
        let count_sql = format!("SELECT COUNT_BIG(*) FROM {source}{condition}");
        let key_sql = primary_key_sql(&request.database);
        self.with_main(|client| {
            Box::pin(async move {
                let started = Instant::now();
                let page = read_result_sets(client, &page_sql, usize::MAX)
                    .await?
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let elapsed_ms = elapsed_millis(started);
                let total = fetch(client, &count_sql, &[])
                    .await?
                    .first()
                    .map(|row| row.text(0))
                    .unwrap_or_default();
                let primary_key = fetch(client, &key_sql, &[&schema, &name])
                    .await?
                    .iter()
                    .map(|row| row.text(0))
                    .collect();
                Ok(TablePage {
                    columns: page.columns,
                    rows: page.rows,
                    total: total.parse().unwrap_or(0),
                    primary_key,
                    elapsed_ms,
                })
            })
        })
        .await
    }

    async fn apply_changes(&self, database: &str, table: &str, changes: Vec<RowChange>) -> AppResult<u64> {
        let (schema, name) = split_object(table);
        let target = format!("{}.{}.{}", bracket(database), bracket(&schema), bracket(&name));
        let statements: Vec<(String, Vec<Option<String>>)> =
            changes.iter().map(|change| statement(change, &target)).collect::<AppResult<_>>()?;
        self.with_main(|client| {
            Box::pin(async move {
                client.simple_query("BEGIN TRANSACTION").await?.into_results().await?;
                let mut affected = 0;
                for (sql, params) in &statements {
                    let bound: Vec<&dyn ToSql> = params.iter().map(|param| param as &dyn ToSql).collect();
                    match client.execute(sql.as_str(), &bound).await {
                        Ok(result) => affected += result.rows_affected().iter().sum::<u64>(),
                        Err(error) => {
                            let _ = client.simple_query("ROLLBACK TRANSACTION").await;
                            return Err(error.into());
                        }
                    }
                }
                client.simple_query("COMMIT TRANSACTION").await?.into_results().await?;
                Ok(affected)
            })
        })
        .await
    }

    async fn open_session(&self, session_id: &str) -> AppResult<()> {
        let client = connect(&self.config).await?;
        lock(&self.sessions).insert(session_id.to_owned(), Arc::new(AsyncMutex::new(client)));
        Ok(())
    }

    fn close_session(&self, session_id: &str) {
        lock(&self.sessions).remove(session_id);
    }

    async fn run_query(&self, session_id: &str, database: Option<String>, sql: String) -> AppResult<QueryOutcome> {
        let session = self.session(session_id)?;
        let mut client = session.lock().await;
        if let Some(database) = database.filter(|name| !name.is_empty()) {
            client
                .simple_query(format!("USE {}", bracket(&database)))
                .await?
                .into_results()
                .await?;
        }
        let started = Instant::now();
        let sets = if is_read_statement(&sql) {
            read_result_sets(&mut client, &sql, MAX_QUERY_ROWS).await?
        } else {
            let result = client.execute(sql.as_str(), &[]).await?;
            vec![ResultSet {
                affected_rows: result.rows_affected().iter().sum(),
                ..ResultSet::default()
            }]
        };
        Ok(QueryOutcome {
            sets,
            elapsed_ms: elapsed_millis(started),
        })
    }

    async fn cancel_query(&self, _session_id: &str) -> AppResult<()> {
        Err(AppError::new("Cancelling a running statement is not supported for SQL Server yet"))
    }

    async fn table_relations(&self, database: &str, table: &str) -> AppResult<TableRelations> {
        let (schema, name) = split_object(table);
        let db = bracket(database);
        let sql = format!(
            "SELECT fk.name, SCHEMA_NAME(tp.schema_id), tp.name, cp.name, SCHEMA_NAME(tr.schema_id), tr.name, cr.name \
             FROM {db}.sys.foreign_keys fk \
             JOIN {db}.sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id \
             JOIN {db}.sys.tables tp ON fkc.parent_object_id = tp.object_id \
             JOIN {db}.sys.columns cp ON cp.object_id = tp.object_id AND cp.column_id = fkc.parent_column_id \
             JOIN {db}.sys.tables tr ON fkc.referenced_object_id = tr.object_id \
             JOIN {db}.sys.columns cr ON cr.object_id = tr.object_id AND cr.column_id = fkc.referenced_column_id \
             WHERE (SCHEMA_NAME(tp.schema_id) = @P1 AND tp.name = @P2) OR (SCHEMA_NAME(tr.schema_id) = @P1 AND tr.name = @P2) \
             ORDER BY SCHEMA_NAME(tp.schema_id), tp.name, fk.name, fkc.constraint_column_id"
        );
        let database = database.to_owned();
        let object = object_name(&schema, &name);
        self.with_main(|client| {
            Box::pin(async move {
                let rows = fetch(client, &sql, &[&schema, &name]).await?;
                // Fold schema into the table name so the UI addresses it the same way as the tree.
                let folded: Vec<TextRow> = rows
                    .into_iter()
                    .map(|row| {
                        TextRow(vec![
                            row.optional(0),
                            Some(database.clone()),
                            Some(object_name(&row.text(1), &row.text(2))),
                            row.optional(3),
                            Some(database.clone()),
                            Some(object_name(&row.text(4), &row.text(5))),
                            row.optional(6),
                        ])
                    })
                    .collect();
                Ok(relations::split_relations(relations::group_relations(&folded), &database, &object))
            })
        })
        .await
    }

    async fn schema_bulk(&self, database: &str) -> AppResult<Option<BulkSchema>> {
        let tables = self.list_tables(database).await?;
        let db = bracket(database);
        let columns_sql = format!(
            "SELECT c.TABLE_SCHEMA, c.TABLE_NAME, c.COLUMN_NAME, c.DATA_TYPE, c.CHARACTER_MAXIMUM_LENGTH, \
                    c.NUMERIC_PRECISION, c.NUMERIC_SCALE, c.IS_NULLABLE \
             FROM {db}.INFORMATION_SCHEMA.COLUMNS c \
             ORDER BY c.TABLE_SCHEMA, c.TABLE_NAME, c.ORDINAL_POSITION"
        );
        let keys_sql = format!(
            "SELECT kcu.TABLE_SCHEMA, kcu.TABLE_NAME, kcu.COLUMN_NAME FROM {db}.INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
             JOIN {db}.INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
               ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME AND tc.TABLE_SCHEMA = kcu.TABLE_SCHEMA AND tc.TABLE_NAME = kcu.TABLE_NAME \
             WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'"
        );
        let relations_sql = format!(
            "SELECT fk.name, SCHEMA_NAME(tp.schema_id), tp.name, cp.name, SCHEMA_NAME(tr.schema_id), tr.name, cr.name \
             FROM {db}.sys.foreign_keys fk \
             JOIN {db}.sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id \
             JOIN {db}.sys.tables tp ON fkc.parent_object_id = tp.object_id \
             JOIN {db}.sys.columns cp ON cp.object_id = tp.object_id AND cp.column_id = fkc.parent_column_id \
             JOIN {db}.sys.tables tr ON fkc.referenced_object_id = tr.object_id \
             JOIN {db}.sys.columns cr ON cr.object_id = tr.object_id AND cr.column_id = fkc.referenced_column_id \
             ORDER BY SCHEMA_NAME(tp.schema_id), tp.name, fk.name, fkc.constraint_column_id"
        );
        let database = database.to_owned();
        self.with_main(|client| {
            Box::pin(async move {
                let column_rows = fetch(client, &columns_sql, &[]).await?;
                let keys: HashSet<(String, String, String)> = fetch(client, &keys_sql, &[])
                    .await?
                    .iter()
                    .map(|row| (row.text(0), row.text(1), row.text(2)))
                    .collect();
                let relation_rows = fetch(client, &relations_sql, &[]).await?;
                let mut columns: HashMap<String, Vec<SchemaColumn>> = HashMap::new();
                for row in &column_rows {
                    let (schema, table, name) = (row.text(0), row.text(1), row.text(2));
                    let primary = keys.contains(&(schema.clone(), table.clone(), name.clone()));
                    columns.entry(object_name(&schema, &table)).or_default().push(SchemaColumn {
                        column_type: type_label(&row.text(3), row.optional(4), row.optional(5), row.optional(6)),
                        nullable: row.text(7) == "YES",
                        primary,
                        name,
                    });
                }
                // Fold schema into the table name so the UI addresses it the same way as the tree.
                let folded: Vec<TextRow> = relation_rows
                    .into_iter()
                    .map(|row| {
                        TextRow(vec![
                            row.optional(0),
                            Some(database.clone()),
                            Some(object_name(&row.text(1), &row.text(2))),
                            row.optional(3),
                            Some(database.clone()),
                            Some(object_name(&row.text(4), &row.text(5))),
                            row.optional(6),
                        ])
                    })
                    .collect();
                let relations = relations::group_relations(&folded);
                Ok(Some(BulkSchema {
                    tables,
                    columns,
                    relations,
                }))
            })
        })
        .await
    }

    async fn close(&self) {
        lock(&self.sessions).clear();
        *self.main.lock().await = None;
    }
}

fn build_config(profile: &ConnectionProfile, password: &str) -> Config {
    let mut config = Config::new();
    config.host(&profile.host);
    config.port(profile.port);
    config.authentication(AuthMethod::sql_server(&profile.user, password));
    if let Some(database) = profile.database.as_deref().filter(|name| !name.is_empty()) {
        config.database(database);
    }
    let flag = |key: &str, default: bool| profile.options.get(key).map_or(default, |value| value == "true");
    config.encryption(if flag("encrypt", true) {
        EncryptionLevel::Required
    } else {
        EncryptionLevel::Off
    });
    if flag("trustServerCertificate", false) {
        config.trust_cert();
    }
    config
}

async fn connect(config: &Config) -> AppResult<TdsClient> {
    let connect = async {
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        Ok::<_, AppError>(Client::connect(config.clone(), tcp.compat_write()).await?)
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(result) => result,
        Err(_) => Err(AppError::new(
            "Connection timed out. Is SQL Server reachable on that host and port?",
        )),
    }
}

async fn server_version(client: &mut TdsClient) -> AppResult<String> {
    let rows = fetch(
        client,
        "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)), CAST(SERVERPROPERTY('Edition') AS NVARCHAR(128))",
        &[],
    )
    .await?;
    let row = rows.first();
    let version = row.map(|row| row.text(0)).unwrap_or_default();
    let edition = row.map(|row| row.text(1)).unwrap_or_default();
    Ok(if edition.is_empty() {
        version
    } else {
        format!("{version} ({edition})")
    })
}

/// Runs a parameterised statement and returns every row of the first result set as text.
async fn fetch(client: &mut TdsClient, sql: &str, params: &[&dyn ToSql]) -> AppResult<Vec<TextRow>> {
    let rows: Vec<Row> = client.query(sql, params).await?.into_first_result().await?;
    Ok(rows
        .into_iter()
        .map(|row| TextRow(row.into_iter().map(|data| cell_text(&data)).collect()))
        .collect())
}

/// Streams every result set of a batch, keeping column metadata.
async fn read_result_sets(client: &mut TdsClient, sql: &str, max_rows: usize) -> AppResult<Vec<ResultSet>> {
    let mut stream = client.simple_query(sql).await?;
    let mut sets: Vec<ResultSet> = Vec::new();
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(meta) => sets.push(ResultSet {
                columns: meta.columns().iter().map(column_meta).collect(),
                ..ResultSet::default()
            }),
            QueryItem::Row(row) => {
                if sets.is_empty() {
                    sets.push(ResultSet {
                        columns: row.columns().iter().map(column_meta).collect(),
                        ..ResultSet::default()
                    });
                }
                let set = sets.last_mut().expect("a metadata item precedes rows");
                if set.rows.len() < max_rows {
                    set.rows.push(row.into_iter().map(|data| cell_text(&data)).collect());
                } else {
                    set.truncated = true;
                }
            }
        }
    }
    if sets.is_empty() {
        sets.push(ResultSet::default());
    }
    Ok(sets)
}

fn column_meta(column: &tiberius::Column) -> ColumnMeta {
    let column_type = column.column_type();
    ColumnMeta {
        name: column.name().to_owned(),
        type_name: format!("{column_type:?}").to_lowercase(),
        numeric: is_numeric(column_type),
        binary: matches!(column_type, ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image),
        primary: false,
        vector_dims: None,
    }
}

fn is_numeric(column_type: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        column_type,
        Int1 | Int2 | Int4 | Int8 | Intn | Float4 | Float8 | Floatn | Money | Money4 | Decimaln | Numericn
    )
}

#[allow(unreachable_patterns)]
fn cell_text(data: &ColumnData<'static>) -> Cell {
    match data {
        ColumnData::U8(value) => value.map(|number| number.to_string()),
        ColumnData::I16(value) => value.map(|number| number.to_string()),
        ColumnData::I32(value) => value.map(|number| number.to_string()),
        ColumnData::I64(value) => value.map(|number| number.to_string()),
        ColumnData::F32(value) => value.map(|number| number.to_string()),
        ColumnData::F64(value) => value.map(|number| number.to_string()),
        ColumnData::Bit(value) => value.map(|bit| if bit { "1" } else { "0" }.to_owned()),
        ColumnData::String(value) => value.as_ref().map(|text| text.to_string()),
        ColumnData::Guid(value) => value.map(|guid| guid.to_string()),
        ColumnData::Binary(value) => value.as_ref().map(|bytes| encode_hex(bytes)),
        ColumnData::Numeric(value) => value.map(|number| number.to_string()),
        ColumnData::Xml(value) => value.as_ref().map(|xml| xml.as_ref().to_string()),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            NaiveDateTime::from_sql(data).ok().flatten().map(|value| value.to_string())
        }
        ColumnData::Date(_) => NaiveDate::from_sql(data).ok().flatten().map(|value| value.to_string()),
        ColumnData::Time(_) => NaiveTime::from_sql(data).ok().flatten().map(|value| value.to_string()),
        ColumnData::DateTimeOffset(_) => DateTime::<FixedOffset>::from_sql(data)
            .ok()
            .flatten()
            .map(|value| value.to_rfc3339()),
        _ => Some(format!("{data:?}")),
    }
}

fn statement(change: &RowChange, target: &str) -> AppResult<(String, Vec<Option<String>>)> {
    let mut params: Vec<Option<String>> = Vec::new();
    let mut placeholder = |value: &Option<String>| {
        params.push(value.clone());
        format!("@P{}", params.len())
    };
    let sql = match change {
        RowChange::Insert { values } => {
            let columns: Vec<String> = values.iter().map(|cell| bracket(&cell.column)).collect();
            let holes: Vec<String> = values.iter().map(|cell| placeholder(&cell.value)).collect();
            format!("INSERT INTO {target} ({}) VALUES ({})", columns.join(", "), holes.join(", "))
        }
        RowChange::Update { key, values } => {
            let assignments: Vec<String> = values
                .iter()
                .map(|cell| format!("{} = {}", bracket(&cell.column), placeholder(&cell.value)))
                .collect();
            let condition = key_condition(key, &mut placeholder)?;
            format!("UPDATE TOP (1) {target} SET {} WHERE {condition}", assignments.join(", "))
        }
        RowChange::Delete { key } => {
            let condition = key_condition(key, &mut placeholder)?;
            format!("DELETE TOP (1) FROM {target} WHERE {condition}")
        }
    };
    Ok((sql, params))
}

fn key_condition(key: &[CellValue], placeholder: &mut impl FnMut(&Option<String>) -> String) -> AppResult<String> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the row"));
    }
    let parts: Vec<String> = key
        .iter()
        .map(|cell| match &cell.value {
            None => format!("{} IS NULL", bracket(&cell.column)),
            Some(_) => format!("{} = {}", bracket(&cell.column), placeholder(&cell.value)),
        })
        .collect();
    Ok(parts.join(" AND "))
}

fn primary_key_sql(database: &str) -> String {
    format!(
        "SELECT kcu.COLUMN_NAME FROM {db}.INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
         JOIN {db}.INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
           ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME AND tc.TABLE_SCHEMA = kcu.TABLE_SCHEMA AND tc.TABLE_NAME = kcu.TABLE_NAME \
         WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY' AND tc.TABLE_SCHEMA = @P1 AND tc.TABLE_NAME = @P2 \
         ORDER BY kcu.ORDINAL_POSITION",
        db = bracket(database)
    )
}

fn type_label(data_type: &str, length: Cell, precision: Cell, scale: Cell) -> String {
    match (length, precision, scale) {
        (Some(length), _, _) if length == "-1" => format!("{data_type}(max)"),
        (Some(length), _, _) => format!("{data_type}({length})"),
        (None, Some(precision), Some(scale)) if matches!(data_type, "decimal" | "numeric") => format!("{data_type}({precision},{scale})"),
        _ => data_type.to_owned(),
    }
}

/// SQL Server has no SHOW CREATE TABLE; this is a readable approximation.
fn synthesize_ddl(qualified: &str, columns: &[ColumnDesign], keys: &[String]) -> String {
    let mut lines: Vec<String> = columns
        .iter()
        .map(|column| {
            let mut line = format!("  {} {}", bracket(&column.name), column.column_type);
            if column.extra == "identity" {
                line.push_str(" IDENTITY(1,1)");
            }
            line.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
            if let Some(default) = &column.default_value {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();
    if !keys.is_empty() {
        let list: Vec<String> = keys.iter().map(|key| bracket(key)).collect();
        lines.push(format!("  PRIMARY KEY ({})", list.join(", ")));
    }
    format!("CREATE TABLE {qualified} (\n{}\n);", lines.join(",\n"))
}

fn is_read_statement(sql: &str) -> bool {
    let first = sql
        .trim_start()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    READ_KEYWORDS.iter().any(|keyword| first.starts_with(keyword))
}

fn where_clause(filter: Option<&str>) -> String {
    filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .map(|filter| format!(" WHERE {filter}"))
        .unwrap_or_default()
}

pub(crate) fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn object_name(schema: &str, name: &str) -> String {
    if schema == DEFAULT_SCHEMA {
        name.to_owned()
    } else {
        format!("{schema}.{name}")
    }
}

fn split_object(table: &str) -> (String, String) {
    match table.split_once('.') {
        Some((schema, name)) => (schema.to_owned(), name.to_owned()),
        None => (DEFAULT_SCHEMA.to_owned(), table.to_owned()),
    }
}
