//! Browsing and editing rows for the contract. Browse SQL comes from `engines::common::sqlbuild` with every value bound.
//! Tables without a primary key are edited by their `rowid` (browsed as the pseudo column `_rowid_`); views have no key
//! and are read-only. SQLite has no row versions, so updates and deletes compare the original values (`IS`, NULL-safe).
//! Edits run in one transaction; a change that matches no row rolls everything back with a `CONFLICT`.

use std::time::Instant;

use rusqlite::{params_from_iter, Connection};

use super::{bind_params, browse_columns, describe_sync, graph, lock, map_err, nodes, row_values, SqliteDialect, SqliteEngine};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        ApplyResult, BrowseRequest, ColumnInfo, Field, Filter, NodeKind, ObjectDescription, Page, RowChange, RowChangeSet, Total, Value,
    },
    engines::common::{
        self, adapt,
        sqlbuild::{self, SqlDialect},
        ValueKind,
    },
    error::{AppError, AppResult},
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;

impl SqliteEngine {
    /// Runs blocking work on the engine's connection (the contract's errors are `AppError`s).
    async fn on_connection<T: Send + 'static>(&self, work: impl FnOnce(&mut Connection) -> AppResult<T> + Send + 'static) -> AppResult<T> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || work(&mut lock(&conn)))
            .await
            .map_err(|error| AppError::new(format!("The worker thread failed: {error}")))?
    }

    pub(super) async fn ping(&self) -> AppResult<()> {
        self.on_connection(|c| {
            c.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)).map_err(map_err)?;
            Ok(())
        })
        .await
    }

    pub(super) async fn describe_contract(&self, object: &crate::contract::ObjectRef) -> AppResult<ObjectDescription> {
        let object = nodes::to_core(object)?;
        self.on_connection(move |c| {
            let core = describe_sync(c, &object)?;
            let database = core.object.schema.clone().unwrap_or_else(|| "main".to_owned());
            let mut description = adapt::description_to_contract(core, None);
            description.object = nodes::to_contract(&common::ObjectRef::new(
                None,
                Some(&database),
                &description.object.name,
                to_core_kind(&description.object.kind),
            ));
            // Foreign keys stay inside one attached database: name it as the database, not as a schema.
            for key in &mut description.foreign_keys {
                key.ref_database = key.ref_schema.take();
            }
            if description.object.kind == NodeKind::TABLE {
                description.referenced_by = graph::referencing(c, &database, &description.object.name)?;
            }
            Ok(description)
        })
        .await
    }

    pub(super) async fn graph(&self, database: &str) -> AppResult<crate::contract::SchemaGraph> {
        let database = database.to_owned();
        self.on_connection(move |c| Ok(graph::schema_graph(c, &database)?)).await
    }

    pub(super) async fn browse_page(&self, request: &BrowseRequest) -> AppResult<Page> {
        let request = request.clone();
        self.on_connection(move |c| browse_sync(c, &request)).await
    }

    pub(super) async fn apply_set(&self, set: &RowChangeSet) -> AppResult<ApplyResult> {
        let set = set.clone();
        self.on_connection(move |c| apply_sync(c, &set)).await
    }
}

fn to_core_kind(kind: &NodeKind) -> common::NodeKind {
    common::NodeKind::try_from(kind).unwrap_or(common::NodeKind::Table)
}

fn browse_sync(c: &Connection, request: &BrowseRequest) -> AppResult<Page> {
    let started = Instant::now();
    let object = nodes::to_core(&request.object)?;
    let (description, columns, key) = browse_columns(c, &object)?;
    let shown: Vec<ColumnInfo> = match &request.columns {
        Some(wanted) => columns
            .into_iter()
            .filter(|column| wanted.contains(&column.name) || key.contains(&column.name))
            .collect(),
        None => columns,
    };
    let raw = match &request.filter {
        Filter::Raw { text, .. } => Some(text.clone()),
        _ => None,
    };
    let mut core = adapt::browse_to_common(&BrowseRequest {
        filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
        ..request.clone()
    })?;
    core.object = description.object.clone();
    core.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
    let built = sqlbuild::build_browse_with_raw(&SqliteDialect, &core, &shown, &key, raw.as_deref())?;
    let mut statement = c.prepare(&built.sql).map_err(map_err)?;
    let kinds: Vec<ValueKind> = shown.iter().map(|column| column.kind.clone()).collect();
    let mut rows_out = Vec::new();
    let mut rows = statement.query(params_from_iter(bind_params(&built.params))).map_err(map_err)?;
    while let Some(row) = rows.next().map_err(map_err)? {
        rows_out.push(row_values(row, &kinds)?);
    }
    drop(rows);
    let total = if request.exact_total {
        let where_clause = if built.where_sql.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", built.where_sql)
        };
        // The keyset parameters come after the filter's.
        let filter_params = built.params.len() - request.after.as_ref().map_or(0, |after| after.len() * (after.len() + 1) / 2);
        let sql = format!(
            "SELECT COUNT(*) FROM {}{where_clause}",
            SqliteDialect.table_sql(&description.object)
        );
        let count: i64 = c
            .query_row(&sql, params_from_iter(bind_params(&built.params[..filter_params])), |row| {
                row.get(0)
            })
            .map_err(map_err)?;
        Some(Total {
            value: count.max(0) as u64,
            exact: true,
        })
    } else if request.estimate_total && built.where_sql.is_empty() {
        // SQLite keeps no planner estimates: only ANALYZE's statistics.
        description.row_estimate.map(|rows| Total {
            value: rows.max(0) as u64,
            exact: false,
        })
    } else {
        None
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let mut page = adapt::page_to_contract(sqlbuild::finish_page(shown, rows_out, core.limit, &built, key, None), elapsed_ms);
    page.total = total;
    Ok(page)
}

fn apply_sync(c: &mut Connection, set: &RowChangeSet) -> AppResult<ApplyResult> {
    let object = nodes::to_core(&set.object)?;
    let (description, columns, key) = browse_columns(c, &object)?;
    if description.object.kind != common::NodeKind::Table {
        return Err(AppError::new(format!(
            "{} is not a table, so its rows are read-only.",
            description.object.name
        )));
    }
    if key.is_empty() {
        return Err(AppError::new(format!(
            "{} has no primary key and no rowid, so its rows cannot be changed safely.",
            description.object.name
        )));
    }
    let statements = set
        .changes
        .iter()
        .map(|change| sqlbuild::build_guarded(&SqliteDialect, &description.object, change, &columns, None).map_err(AppError::from))
        .collect::<AppResult<Vec<_>>>()?;
    let mut result = ApplyResult::default();
    let mut missed = Vec::new();
    let tx = c.transaction().map_err(map_err)?;
    for (index, (change, built)) in set.changes.iter().zip(&statements).enumerate() {
        let affected = tx
            .execute(&built.sql, params_from_iter(bind_params(&built.params)))
            .map_err(map_err)? as u64;
        match change {
            RowChange::Insert { .. } => result.inserted += affected,
            RowChange::Update { .. } if affected == 0 => missed.push(index),
            RowChange::Update { .. } => result.updated += affected,
            RowChange::Delete { .. } if affected == 0 => missed.push(index),
            RowChange::Delete { .. } => result.deleted += affected,
        }
    }
    if missed.is_empty() {
        tx.commit().map_err(map_err)?;
        return Ok(result);
    }
    tx.rollback().map_err(map_err)?;
    let mut conflicts = Vec::with_capacity(missed.len());
    for index in missed {
        let change = &set.changes[index];
        let current = reread(c, &description.object, change.key(), &columns)?;
        conflicts.push(RowConflict::new(index, change, current));
    }
    Err(conflict_error(&set.object, conflicts))
}

/// The row with this key as it is now (all columns), or `None` when it no longer exists.
fn reread(c: &Connection, object: &common::ObjectRef, key: &[Field], columns: &[ColumnInfo]) -> AppResult<Option<Vec<Field>>> {
    let built = sqlbuild::build_reread(&SqliteDialect, object, key, columns)?;
    let kinds: Vec<ValueKind> = columns.iter().map(|column| column.kind.clone()).collect();
    let mut statement = c.prepare(&built.sql).map_err(map_err)?;
    let mut rows = statement.query(params_from_iter(bind_params(&built.params))).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        Some(row) => {
            let values: Vec<Value> = row_values(row, &kinds)?;
            Ok(Some(
                columns
                    .iter()
                    .zip(values)
                    .map(|(column, value)| Field::new(&column.name, value))
                    .collect(),
            ))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ObjectRef;

    fn connection() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT);
             CREATE TABLE loose (name TEXT, qty INTEGER);
             CREATE TABLE wr (code TEXT PRIMARY KEY, qty INTEGER) WITHOUT ROWID;
             CREATE VIEW v AS SELECT * FROM items;
             INSERT INTO items VALUES (1, 'a', NULL), (2, 'b', 'x');
             INSERT INTO loose VALUES ('p', 1), ('q', 2);
             INSERT INTO wr VALUES ('k', 1);",
        )
        .unwrap();
        c
    }

    fn object(name: &str, kind: NodeKind) -> ObjectRef {
        ObjectRef::new(Some("main"), None, name, kind)
    }

    fn update(table: &str, key: (&str, Value), name: &str, original: Value) -> RowChangeSet {
        RowChangeSet {
            object: object(table, NodeKind::TABLE),
            changes: vec![RowChange::Update {
                key: vec![Field::new(key.0, key.1)],
                values: vec![Field::new(name, Value::Text("new".into()))],
                original: vec![Field::new(name, original)],
                version: None,
                force: false,
            }],
        }
    }

    #[test]
    fn rows_are_browsed_with_a_raw_filter_and_counted_exactly() {
        let c = connection();
        let mut request = BrowseRequest::new(object("items", NodeKind::TABLE), 10);
        request.filter = Filter::Raw {
            language: crate::contract::RawFilterLang::Sql,
            text: "id > 1".into(),
        };
        request.exact_total = true;
        let page = browse_sync(&c, &request).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.key_columns, ["id"]);
        assert_eq!(page.total, Some(Total { value: 1, exact: true }));
        assert!(page.row_versions.is_none());
        let loose = browse_sync(&c, &BrowseRequest::new(object("loose", NodeKind::TABLE), 10)).unwrap();
        assert_eq!(loose.key_columns, ["_rowid_"], "a table without a key is keyed by rowid");
        assert_eq!(loose.columns[0].name, "_rowid_");
    }

    #[test]
    fn updates_check_the_original_values_and_a_stale_one_is_a_conflict() {
        let mut c = connection();
        let applied = apply_sync(&mut c, &update("items", ("id", Value::Int(1)), "name", Value::Text("a".into()))).unwrap();
        assert_eq!(applied.updated, 1);
        let error = apply_sync(&mut c, &update("items", ("id", Value::Int(1)), "name", Value::Text("a".into()))).unwrap_err();
        assert_eq!(error.code(), Some(crate::contract::errors::CONFLICT));
        let conflict = &error.field("conflicts").unwrap()[0];
        assert_eq!(conflict["current"][1]["value"]["v"], "new");
        // NULLs compare as equal: the original NULL note matches.
        let by_null = update("items", ("id", Value::Int(1)), "note", Value::Null);
        assert_eq!(apply_sync(&mut c, &by_null).unwrap().updated, 1);
        // A row that is gone is a conflict without a current row.
        let gone = update("items", ("id", Value::Int(99)), "name", Value::Text("x".into()));
        let error = apply_sync(&mut c, &gone).unwrap_err();
        assert!(error.field("conflicts").unwrap()[0]["current"].is_null());
    }

    #[test]
    fn rowid_tables_are_edited_by_rowid_and_views_are_read_only() {
        let mut c = connection();
        let by_rowid = update("loose", ("_rowid_", Value::Int(2)), "name", Value::Text("q".into()));
        assert_eq!(apply_sync(&mut c, &by_rowid).unwrap().updated, 1);
        let on_view = update("v", ("id", Value::Int(1)), "name", Value::Text("a".into()));
        let mut set = on_view;
        set.object = object("v", NodeKind::VIEW);
        assert!(apply_sync(&mut c, &set).unwrap_err().to_string().contains("read-only"));
        let by_key = update("wr", ("code", Value::Text("k".into())), "qty", Value::Int(1));
        let mut set = by_key;
        set.changes = vec![RowChange::Delete {
            key: vec![Field::new("code", Value::Text("k".into()))],
            original: Vec::new(),
            version: None,
            force: false,
        }];
        assert_eq!(
            apply_sync(&mut c, &set).unwrap().deleted,
            1,
            "WITHOUT ROWID tables are edited by their primary key"
        );
    }
}
