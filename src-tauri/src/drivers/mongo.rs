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
    bson::{doc, oid::ObjectId, Bson, DateTime, Decimal128, Document},
    options::{ClientOptions, Credential, ServerAddress},
    Client, ClientSession, Collection, Database,
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
                        let value = coerce_cell(&cell.column, &cell.value, None)?;
                        document.insert(cell.column, value);
                    }
                    collection.insert_one(document).await?;
                    affected += 1;
                }
                RowChange::Update { key, values } => {
                    // Edited cells arrive as text; the stored document says which BSON type each field keeps.
                    let filter = key_filter(&key)?;
                    let original = collection.find_one(filter.clone()).await?.unwrap_or_default();
                    let mut set = Document::new();
                    for cell in values {
                        let value = coerce_cell(&cell.column, &cell.value, original.get(&cell.column))?;
                        set.insert(cell.column, value);
                    }
                    let result = collection.update_one(filter, doc! { "$set": set }).await?;
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
        let db = self.client.database(&database);
        // getMore must run in the session that created the cursor, so the command gets an explicit one.
        let mut session = self.client.start_session().await?;
        let reply = db.run_command(command).session(&mut session).await?;
        let set = match reply.get_document("cursor").ok() {
            Some(cursor) if cursor.get_array("firstBatch").is_ok() => {
                let (documents, truncated) = drain_cursor(&db, &mut session, cursor, limits).await?;
                let mut set = documents_to_set(&documents);
                set.truncated = truncated;
                set
            }
            _ => documents_to_set(std::slice::from_ref(&reply)),
        };
        let elapsed_ms = elapsed_millis(started);
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

/// Reads a command cursor to its end (the command's own `limit` ends it) or to `max_rows`, with getMore
/// after the first batch, and closes it when rows are left over. Returns the documents and whether some were cut.
async fn drain_cursor(db: &Database, session: &mut ClientSession, cursor: &Document, limits: &Limits) -> AppResult<(Vec<Document>, bool)> {
    let first = cursor.get_array("firstBatch").map_err(|error| AppError::new(error.to_string()))?;
    let mut documents: Vec<Document> = first.iter().filter_map(|item| item.as_document().cloned()).collect();
    let mut id = cursor_id(cursor);
    let collection = cursor
        .get_str("ns")
        .ok()
        .and_then(|ns| ns.split_once('.'))
        .map(|(_, name)| name.to_owned())
        .unwrap_or_default();
    while id != 0 && documents.len() <= limits.max_rows && !collection.is_empty() {
        let wanted = i64::try_from(limits.max_rows + 1 - documents.len()).unwrap_or(i64::MAX);
        let mut get_more = doc! { "getMore": id, "collection": collection.as_str(), "batchSize": wanted };
        if limits.timeout_secs > 0 {
            get_more.insert("maxTimeMS", i64::from(limits.timeout_secs) * 1000);
        }
        let reply = db.run_command(get_more).session(&mut *session).await?;
        let next = reply.get_document("cursor").map_err(|error| AppError::new(error.to_string()))?;
        if let Ok(batch) = next.get_array("nextBatch") {
            documents.extend(batch.iter().filter_map(|item| item.as_document().cloned()));
        }
        id = cursor_id(next);
    }
    if id != 0 && !collection.is_empty() {
        // Release the server-side cursor instead of leaving it to time out.
        let _ = db
            .run_command(doc! { "killCursors": collection.as_str(), "cursors": [id] })
            .session(&mut *session)
            .await;
    }
    let truncated = documents.len() > limits.max_rows || id != 0;
    documents.truncate(limits.max_rows);
    Ok((documents, truncated))
}

fn cursor_id(cursor: &Document) -> i64 {
    match cursor.get("id") {
        Some(Bson::Int64(id)) => *id,
        Some(Bson::Int32(id)) => i64::from(*id),
        _ => 0,
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

/// A cell without a stored type to follow (new documents, new fields, key values): JSON and Extended
/// JSON (`{"$date": …}`, `{"$oid": …}`) parse to the matching BSON, anything else stays a string.
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
        if let Some(bson) = parse_extended_json(trimmed) {
            return bson;
        }
    }
    Bson::String(text.clone())
}

/// Relaxed Extended JSON writes every number as plain JSON, so a small long reads back as an int and a
/// whole double as an int. Where the stored value at the same place was a long or a double, the edited
/// number takes that type again.
fn match_numbers(edited: Bson, original: &Bson) -> Bson {
    match (edited, original) {
        (Bson::Document(edited), Bson::Document(original)) => Bson::Document(
            edited
                .into_iter()
                .map(|(key, value)| {
                    let value = match original.get(&key) {
                        Some(before) => match_numbers(value, before),
                        None => value,
                    };
                    (key, value)
                })
                .collect(),
        ),
        (Bson::Array(edited), Bson::Array(original)) => Bson::Array(
            edited
                .into_iter()
                .enumerate()
                .map(|(index, value)| match original.get(index) {
                    Some(before) => match_numbers(value, before),
                    None => value,
                })
                .collect(),
        ),
        (Bson::Int32(number), Bson::Int64(_)) => Bson::Int64(i64::from(number)),
        (Bson::Int32(number), Bson::Double(_)) => Bson::Double(f64::from(number)),
        // Whole doubles beyond i32 read back as longs; the conversion is exact below 2^53.
        #[allow(clippy::cast_precision_loss)]
        (Bson::Int64(number), Bson::Double(_)) => Bson::Double(number as f64),
        (edited, _) => edited,
    }
}

/// Relaxed or canonical Extended JSON, the format nested values are shown in, back to BSON with its types.
fn parse_extended_json(text: &str) -> Option<Bson> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    Bson::try_from(value).ok()
}

/// An edited cell converted to the BSON type the field already has, so editing a date, a long or an
/// ObjectId keeps it one. Explicit Extended JSON (`{"$numberLong": "5"}`) is accepted for any field.
fn coerce_cell(column: &str, value: &Cell, original: Option<&Bson>) -> AppResult<Bson> {
    let Some(text) = value else {
        return Ok(Bson::Null);
    };
    let trimmed = text.trim();
    let typed = match original {
        None | Some(Bson::Null) => return Ok(cell_to_bson(value)),
        Some(Bson::String(_)) => return Ok(Bson::String(text.clone())),
        Some(Bson::DateTime(_)) => DateTime::parse_rfc3339_str(trimmed).ok().map(Bson::DateTime),
        Some(Bson::Int32(_)) => trimmed.parse::<i32>().ok().map(Bson::Int32),
        Some(Bson::Int64(_)) => trimmed.parse::<i64>().ok().map(Bson::Int64),
        Some(Bson::Double(_)) => trimmed.parse::<f64>().ok().map(Bson::Double),
        Some(Bson::Decimal128(_)) => trimmed.parse::<Decimal128>().ok().map(Bson::Decimal128),
        Some(Bson::Boolean(_)) => match trimmed {
            "true" => Some(Bson::Boolean(true)),
            "false" => Some(Bson::Boolean(false)),
            _ => None,
        },
        Some(Bson::ObjectId(_)) => ObjectId::parse_str(trimmed).ok().map(Bson::ObjectId),
        // Documents, arrays, binary and the rest are shown (and edited) as Extended JSON.
        Some(_) => None,
    };
    if let Some(bson) = typed {
        return Ok(bson);
    }
    let explicit = trimmed.starts_with('{') || trimmed.starts_with('[');
    if explicit {
        if let Some(bson) = parse_extended_json(trimmed) {
            return Ok(match original {
                Some(original @ (Bson::Document(_) | Bson::Array(_))) => match_numbers(bson, original),
                _ => bson,
            });
        }
    }
    let kind = original.map(bson_type).unwrap_or("value");
    let example = match kind {
        "date" => "2025-01-02T21:36:00Z",
        "int" | "long" => "42",
        "double" | "decimal" => "4.2",
        "bool" => "true or false",
        "objectId" => "a 24-digit hex ObjectId",
        _ => "valid JSON",
    };
    Err(AppError::new(format!(
        "{column} holds a {kind}; enter it as {example}, or as Extended JSON (for example {{\"$date\": \"2025-01-02T21:36:00Z\"}}) to change its type."
    )))
}

fn documents_to_set(documents: &[Document]) -> ResultSet {
    let mut order: Vec<String> = vec![ID_FIELD.to_owned()];
    let mut numeric: HashMap<String, bool> = HashMap::new();
    let mut kinds: HashMap<String, Vec<&'static str>> = HashMap::new();
    for document in documents {
        for (key, value) in document {
            if !order.contains(key) {
                order.push(key.clone());
            }
            let kind = bson_type(value);
            let seen = kinds.entry(key.clone()).or_default();
            if kind != "null" && !seen.contains(&kind) {
                seen.push(kind);
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
            // The BSON type(s) seen on this page, e.g. "date" or "int | string".
            type_name: kinds
                .get(key)
                .filter(|kinds| !kinds.is_empty())
                .map_or_else(|| "null".to_owned(), |kinds| kinds.join(" | ")),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Cell {
        Some(value.to_owned())
    }

    #[test]
    fn edits_keep_the_stored_bson_type() {
        let date = DateTime::parse_rfc3339_str("2025-01-02T21:36:00Z").unwrap();
        let shown = bson_text(&Bson::DateTime(date)).unwrap();
        assert_eq!(
            coerce_cell("signedUpAt", &Some(shown), Some(&Bson::DateTime(date))).unwrap(),
            Bson::DateTime(date)
        );
        let edited = coerce_cell("signedUpAt", &text("2026-03-04T05:06:07.123Z"), Some(&Bson::DateTime(date))).unwrap();
        assert!(matches!(edited, Bson::DateTime(_)), "{edited:?}");

        assert_eq!(coerce_cell("n", &text("5"), Some(&Bson::Int64(1))).unwrap(), Bson::Int64(5));
        assert_eq!(coerce_cell("n", &text("5"), Some(&Bson::Int32(1))).unwrap(), Bson::Int32(5));
        assert_eq!(coerce_cell("x", &text("2"), Some(&Bson::Double(1.5))).unwrap(), Bson::Double(2.0));
        assert_eq!(
            coerce_cell("ok", &text("false"), Some(&Bson::Boolean(true))).unwrap(),
            Bson::Boolean(false)
        );
        // A string field stays a string even when the text looks like a number.
        assert_eq!(
            coerce_cell("zip", &text("01234"), Some(&Bson::String("x".into()))).unwrap(),
            Bson::String("01234".into())
        );
        let id = ObjectId::new();
        assert_eq!(
            coerce_cell("ref", &text(&id.to_hex()), Some(&Bson::ObjectId(ObjectId::new()))).unwrap(),
            Bson::ObjectId(id)
        );
        let price: Decimal128 = "9.99".parse().unwrap();
        assert_eq!(
            coerce_cell("price", &text("9.99"), Some(&Bson::Decimal128(price))).unwrap(),
            Bson::Decimal128(price)
        );
        assert_eq!(coerce_cell("gone", &None, Some(&Bson::Int32(1))).unwrap(), Bson::Null);
    }

    #[test]
    fn nested_values_round_trip_through_extended_json() {
        let date = DateTime::parse_rfc3339_str("2025-01-02T21:36:00Z").unwrap();
        let nested = Bson::Document(doc! { "at": date, "n": 3_i64, "tags": ["a", "b"] });
        let shown = bson_text(&nested).unwrap();
        assert_eq!(coerce_cell("meta", &Some(shown), Some(&nested)).unwrap(), nested);
    }

    #[test]
    fn wrong_text_for_a_typed_field_is_refused_with_a_hint() {
        let date = Bson::DateTime(DateTime::now());
        let error = coerce_cell("signedUpAt", &text("tomorrow"), Some(&date)).unwrap_err();
        assert!(error.to_string().contains("signedUpAt holds a date"), "{error}");
        // Explicit Extended JSON changes the type on purpose.
        assert_eq!(
            coerce_cell("n", &text(r#"{"$numberDouble": "1.5"}"#), Some(&Bson::Int32(1))).unwrap(),
            Bson::Double(1.5)
        );
    }

    #[test]
    fn new_fields_parse_json_and_extended_json() {
        assert!(matches!(
            cell_to_bson(&text(r#"{"$date": "2025-01-02T21:36:00Z"}"#)),
            Bson::DateTime(_)
        ));
        assert_eq!(cell_to_bson(&text("plain")), Bson::String("plain".into()));
        assert!(matches!(cell_to_bson(&text("[1, 2]")), Bson::Array(_)));
    }

    #[test]
    fn columns_report_their_bson_types() {
        let documents = [
            doc! { "_id": 1, "at": DateTime::now(), "mixed": 1 },
            doc! { "_id": 2, "mixed": "x", "at": Bson::Null },
        ];
        let set = documents_to_set(&documents);
        let types: Vec<&str> = set.columns.iter().map(|column| column.type_name.as_str()).collect();
        assert_eq!(types, ["int", "date", "int | string"]);
    }
}
