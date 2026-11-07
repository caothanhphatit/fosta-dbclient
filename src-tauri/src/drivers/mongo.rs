//! MongoDB driver. Collections appear as tables whose columns are the union of
//! the fields seen on the current page; nested values show as JSON. Filters
//! are JSON query documents, and the query tab runs database commands.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, oid::ObjectId, Bson, Document},
    options::{ClientOptions, Credential, ServerAddress},
    Client, Collection,
};

use super::{
    types::{
        elapsed_millis, BrowseRequest, Cell, CellValue, ColumnDesign, ColumnMeta, QueryOutcome, ResultSet, RowChange, TableDesign,
        TableInfo, TablePage, TransactionState,
    },
    Driver, Engine, Limits,
};
use crate::{
    config::ConnectionProfile,
    error::{AppError, AppResult},
    lock::lock,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SAMPLE_SIZE: i64 = 100;
const MAX_PAGE_SIZE: u64 = 5000;
const ID_FIELD: &str = "_id";

pub struct MongoConnection {
    client: Client,
    /// Query tabs have no server-side state here; remembered only for bookkeeping.
    sessions: Mutex<HashMap<String, ()>>,
}

impl MongoConnection {
    pub async fn open(profile: &ConnectionProfile, password: &str) -> AppResult<(Self, String)> {
        let options = build_options(profile, password).await?;
        let client = Client::with_options(options)?;
        let info = client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
        let version = info.get_str("version").unwrap_or_default().to_owned();
        Ok((
            Self {
                client,
                sessions: Mutex::default(),
            },
            version,
        ))
    }

    fn collection(&self, database: &str, name: &str) -> Collection<Document> {
        self.client.database(database).collection::<Document>(name)
    }
}

#[async_trait]
impl Driver for MongoConnection {
    fn engine(&self) -> Engine {
        Engine::MongoDb
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        Ok(self.client.list_database_names().await?)
    }

    async fn create_database(&self, name: &str, first_collection: Option<&str>) -> AppResult<()> {
        let collection = first_collection
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::new("MongoDB creates a database together with its first collection; enter a collection name"))?;
        self.client.database(name).create_collection(collection).await?;
        Ok(())
    }

    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>> {
        let db = self.client.database(database);
        let mut names = db.list_collection_names().await?;
        names.sort();
        let mut tables = Vec::with_capacity(names.len());
        for name in names {
            let rows = db.collection::<Document>(&name).estimated_document_count().await.ok();
            tables.push(TableInfo {
                name,
                kind: "table".to_owned(),
                rows,
                comment: String::new(),
            });
        }
        Ok(tables)
    }

    async fn describe_table(&self, database: &str, table: &str) -> AppResult<TableDesign> {
        let collection = self.collection(database, table);
        let sample: Vec<Document> = collection.find(doc! {}).limit(SAMPLE_SIZE).await?.try_collect().await?;
        let mut order: Vec<String> = Vec::new();
        let mut types: HashMap<String, Vec<&'static str>> = HashMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for document in &sample {
            for (key, value) in document {
                if !order.contains(key) {
                    order.push(key.clone());
                }
                let seen = types.entry(key.clone()).or_default();
                let kind = bson_type(value);
                if !seen.contains(&kind) {
                    seen.push(kind);
                }
                *counts.entry(key.clone()).or_default() += 1;
            }
        }
        let columns = order
            .iter()
            .map(|key| ColumnDesign {
                name: key.clone(),
                column_type: types.get(key).map(|kinds| kinds.join(" | ")).unwrap_or_default(),
                nullable: counts.get(key).copied().unwrap_or(0) < sample.len(),
                key: if key == ID_FIELD { "PRI" } else { "" }.to_owned(),
                default_value: None,
                extra: String::new(),
                comment: format!("in {} of {} sampled", counts.get(key).copied().unwrap_or(0), sample.len()),
            })
            .collect();
        let indexes = self
            .client
            .database(database)
            .run_command(doc! { "listIndexes": table })
            .await
            .map(|reply| pretty_json(&Bson::Document(reply)))
            .unwrap_or_default();
        Ok(TableDesign {
            columns,
            ddl: format!("// Indexes of {database}.{table}\n{indexes}"),
        })
    }

    async fn browse_table(&self, request: &BrowseRequest, limits: &Limits) -> AppResult<TablePage> {
        let collection = self.collection(&request.database, &request.table);
        let filter = parse_filter(request.filter.as_deref())?;
        let limit = i64::try_from(request.limit.clamp(1, MAX_PAGE_SIZE)).unwrap_or(i64::MAX);
        let max_time = (limits.timeout_secs > 0).then(|| Duration::from_secs(u64::from(limits.timeout_secs)));
        let mut find = collection.find(filter.clone()).skip(request.offset).limit(limit);
        if let Some(column) = request.order_by.as_deref() {
            find = find.sort(doc! { column: if request.descending { -1 } else { 1 } });
        }
        if let Some(max_time) = max_time {
            find = find.max_time(max_time);
        }
        let started = Instant::now();
        let documents: Vec<Document> = find.await?.try_collect().await?;
        let elapsed_ms = elapsed_millis(started);
        let mut count = collection.count_documents(filter);
        if let Some(max_time) = max_time {
            count = count.max_time(max_time);
        }
        let total = count.await?;
        let set = documents_to_set(&documents);
        Ok(TablePage {
            columns: set.columns,
            rows: set.rows,
            total,
            primary_key: vec![ID_FIELD.to_owned()],
            elapsed_ms,
        })
    }

    async fn apply_changes(&self, database: &str, table: &str, changes: Vec<RowChange>) -> AppResult<u64> {
        let collection = self.collection(database, table);
        let mut affected = 0;
        for change in changes {
            match change {
                RowChange::Insert { values } => {
                    let mut document = Document::new();
                    for cell in values {
                        document.insert(cell.column, cell_to_bson(&cell.value));
                    }
                    collection.insert_one(document).await?;
                    affected += 1;
                }
                RowChange::Update { key, values } => {
                    let mut set = Document::new();
                    for cell in values {
                        set.insert(cell.column, cell_to_bson(&cell.value));
                    }
                    let result = collection.update_one(key_filter(&key)?, doc! { "$set": set }).await?;
                    affected += result.modified_count;
                }
                RowChange::Delete { key } => {
                    affected += collection.delete_one(key_filter(&key)?).await?.deleted_count;
                }
            }
        }
        Ok(affected)
    }

    async fn open_session(&self, session_id: &str) -> AppResult<()> {
        lock(&self.sessions).insert(session_id.to_owned(), ());
        Ok(())
    }

    fn close_session(&self, session_id: &str) {
        lock(&self.sessions).remove(session_id);
    }

    /// Commands run without a server session, so there is no transaction to report.
    async fn run_query(&self, _session_id: &str, database: Option<String>, text: String, limits: &Limits) -> AppResult<QueryOutcome> {
        let database = database
            .filter(|name| !name.is_empty())
            .ok_or_else(|| AppError::new("Pick a database first: MongoDB commands run against one database."))?;
        let command = parse_document(&text).map_err(|error| {
            AppError::new(format!(
                "MongoDB queries are database commands written as JSON, for example \
                 {{ \"find\": \"products\", \"filter\": {{ \"stock\": 0 }}, \"limit\": 50 }}. ({error})"
            ))
        })?;
        let started = Instant::now();
        let reply = self.client.database(&database).run_command(command).await?;
        let elapsed_ms = elapsed_millis(started);
        let set = match reply
            .get_document("cursor")
            .ok()
            .and_then(|cursor| cursor.get_array("firstBatch").ok())
        {
            Some(batch) => {
                let documents: Vec<Document> = batch.iter().filter_map(|item| item.as_document().cloned()).collect();
                let mut set = documents_to_set(&documents);
                set.truncated = documents.len() > limits.max_rows;
                set.rows.truncate(limits.max_rows);
                set
            }
            None => documents_to_set(std::slice::from_ref(&reply)),
        };
        Ok(QueryOutcome {
            sets: vec![set],
            elapsed_ms,
            transaction: TransactionState::unsupported(),
        })
    }

    async fn cancel_query(&self, _session_id: &str) -> AppResult<()> {
        Err(AppError::new("Cancelling a running command is not supported for MongoDB yet"))
    }

    async fn close(&self) {
        lock(&self.sessions).clear();
        self.client.clone().shutdown().await;
    }
}

async fn build_options(profile: &ConnectionProfile, password: &str) -> AppResult<ClientOptions> {
    let uri = profile
        .options
        .get("uri")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let mut options = match uri {
        Some(uri) => {
            // The dialog stores the URI without its password (that lives in Credential Manager).
            let mut options = ClientOptions::parse(uri).await?;
            if let Some(credential) = options.credential.as_mut() {
                if credential.password.is_none() && !password.is_empty() {
                    credential.password = Some(password.to_owned());
                }
            }
            options
        }
        None => {
            let address = ServerAddress::Tcp {
                host: profile.host.clone(),
                port: Some(profile.port),
            };
            let mut options = ClientOptions::builder().hosts(vec![address]).build();
            if !profile.user.is_empty() {
                let credential = Credential::builder()
                    .username(profile.user.clone())
                    .password(password.to_owned())
                    .source(profile.database.clone().filter(|name| !name.is_empty()))
                    .build();
                options.credential = Some(credential);
            }
            options
        }
    };
    options.app_name = Some("Fosta Studio".to_owned());
    options.connect_timeout = Some(CONNECT_TIMEOUT);
    options.server_selection_timeout = Some(CONNECT_TIMEOUT);
    Ok(options)
}

fn parse_filter(filter: Option<&str>) -> AppResult<Document> {
    match filter.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(Document::new()),
        Some(text) => parse_document(text).map_err(|error| {
            AppError::new(format!(
                "Filter must be a JSON query document, e.g. {{ \"status\": \"ACTIVE\" }}. ({error})"
            ))
        }),
    }
}

fn parse_document(text: &str) -> Result<Document, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    mongodb::bson::to_document(&value).map_err(|error| error.to_string())
}

fn key_filter(key: &[CellValue]) -> AppResult<Document> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the document"));
    }
    let mut filter = Document::new();
    for cell in key {
        filter.insert(
            cell.column.clone(),
            if cell.column == ID_FIELD {
                id_to_bson(&cell.value)
            } else {
                cell_to_bson(&cell.value)
            },
        );
    }
    Ok(filter)
}

/// `_id` cells show ObjectIds as hex; turn them back into ObjectIds when matching.
fn id_to_bson(value: &Cell) -> Bson {
    match value.as_deref().and_then(|text| ObjectId::parse_str(text).ok()) {
        Some(id) => Bson::ObjectId(id),
        None => cell_to_bson(value),
    }
}

/// Edited cells are text: JSON parses to the matching BSON, anything else stays a string.
fn cell_to_bson(value: &Cell) -> Bson {
    let Some(text) = value else {
        return Bson::Null;
    };
    let trimmed = text.trim();
    let looks_structured = trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed == "true"
        || trimmed == "false"
        || trimmed == "null"
        || trimmed.parse::<f64>().is_ok();
    if looks_structured {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Ok(bson) = mongodb::bson::to_bson(&parsed) {
                return bson;
            }
        }
    }
    Bson::String(text.clone())
}

fn documents_to_set(documents: &[Document]) -> ResultSet {
    let mut order: Vec<String> = vec![ID_FIELD.to_owned()];
    let mut numeric: HashMap<String, bool> = HashMap::new();
    for document in documents {
        for (key, value) in document {
            if !order.contains(key) {
                order.push(key.clone());
            }
            let is_number = matches!(value, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_));
            numeric
                .entry(key.clone())
                .and_modify(|flag| *flag &= is_number)
                .or_insert(is_number);
        }
    }
    if documents.iter().all(|document| !document.contains_key(ID_FIELD)) {
        order.remove(0);
    }
    let columns = order
        .iter()
        .map(|key| ColumnMeta {
            name: key.clone(),
            type_name: "bson".to_owned(),
            numeric: numeric.get(key).copied().unwrap_or(false),
            binary: false,
            primary: key == ID_FIELD,
            vector_dims: None,
        })
        .collect();
    let rows = documents
        .iter()
        .map(|document| order.iter().map(|key| document.get(key).and_then(bson_text)).collect())
        .collect();
    ResultSet {
        columns,
        rows,
        affected_rows: 0,
        truncated: false,
    }
}

fn bson_text(value: &Bson) -> Cell {
    match value {
        Bson::Null => None,
        Bson::String(text) => Some(text.clone()),
        Bson::ObjectId(id) => Some(id.to_hex()),
        Bson::Int32(number) => Some(number.to_string()),
        Bson::Int64(number) => Some(number.to_string()),
        Bson::Double(number) => Some(number.to_string()),
        Bson::Boolean(flag) => Some(flag.to_string()),
        Bson::DateTime(date) => Some(date.try_to_rfc3339_string().unwrap_or_else(|_| date.to_string())),
        Bson::Decimal128(number) => Some(number.to_string()),
        other => Some(compact_json(other)),
    }
}

fn bson_type(value: &Bson) -> &'static str {
    match value {
        Bson::Null => "null",
        Bson::String(_) => "string",
        Bson::ObjectId(_) => "objectId",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binary",
        _ => "other",
    }
}

fn compact_json(value: &Bson) -> String {
    value.clone().into_relaxed_extjson().to_string()
}

fn pretty_json(value: &Bson) -> String {
    serde_json::to_string_pretty(&value.clone().into_relaxed_extjson()).unwrap_or_default()
}
