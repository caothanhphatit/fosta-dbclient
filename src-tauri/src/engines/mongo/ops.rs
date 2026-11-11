//! The engine operations the MongoDB views call through `invoke`: `mongo.find`, `mongo.count`, `mongo.distinct` and
//! `mongo.resolveRef`. All of them only read.
//!
//! Filters, projections, sorts, collations, hints and `_id` values are relaxed or canonical Extended JSON, given as JSON
//! values or as JSON text (an empty text is no filter). Operators that run JavaScript on the server (`$where`,
//! `$function`, `$accumulator`) are refused here as well as by the classifier. Every operation runs with a time bound:
//! `maxTimeMS` when given, else the workspace's statement timeout, else [`DEFAULT_MAX_TIME_MS`]; never more than the
//! workspace's timeout or [`MAX_TIME_MS`]. Documents come back as relaxed Extended JSON.
//!
//! - `mongo.find { database, collection, filter?, projection?, sort?, skip?, limit?, collation?, hint?, maxTimeMS?,
//!   after? }` → `{ documents, hasMore, keyset, nextAfter, sort, elapsedMs, statement }`. Without a sort and a hint the
//!   documents follow `_id`, so pages are stable; when the sort is `_id` alone, `nextAfter` (canonical Extended JSON of
//!   the last `_id`) passed back as `after` reads the next page.
//! - `mongo.count { database, collection, filter?, estimated?, limit?, skip?, collation?, hint?, maxTimeMS? }` →
//!   `{ count, estimated, elapsedMs }`. `estimated` reads the collection's metadata and applies only without a filter (a view,
//!   which has none, is counted).
//! - `mongo.distinct { database, collection, field, filter?, limit?, collation?, maxTimeMS? }` →
//!   `{ values, truncated, elapsedMs }`: at most `limit` values (array fields give their elements, like `distinct`),
//!   sorted, for value completion.
//! - `mongo.resolveRef { database, collection, id, projection?, maxTimeMS? }` → `{ document }`, the document with that
//!   `_id` or `null`.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, Bson, Document},
    options::{Collation, Hint},
    Client, Collection,
};
use serde_json::{json, Value as Json};

use super::values::{self, ID_FIELD};
use crate::{
    contract::CallContext,
    engines::common::serverjs::find_denied_operator,
    error::{AppError, AppResult},
};

/// The `invoke` names of this module.
pub const OPERATIONS: [&str; 4] = ["mongo.find", "mongo.count", "mongo.distinct", "mongo.resolveRef"];
/// The arguments that hold query documents (checked for server-side JavaScript).
pub const DOCUMENT_ARGS: [&str; 5] = ["filter", "projection", "sort", "collation", "hint"];

/// Documents per `mongo.find` page when no limit is given, and the most a page returns.
const DEFAULT_FIND_LIMIT: u64 = 50;
const MAX_FIND_LIMIT: u64 = 1000;
/// Values per `mongo.distinct` when no limit is given, and the most it returns.
const DEFAULT_DISTINCT_LIMIT: u64 = 100;
const MAX_DISTINCT_LIMIT: u64 = 1000;
/// The time bound without `maxTimeMS` or a workspace timeout, and the longest one accepted.
pub const DEFAULT_MAX_TIME_MS: u64 = 30_000;
pub const MAX_TIME_MS: u64 = 300_000;

/// Runs one of [`OPERATIONS`].
pub async fn invoke(client: &Client, name: &str, args: &Json, ctx: &CallContext) -> AppResult<Json> {
    if !OPERATIONS.contains(&name) {
        return Err(crate::contract::unsupported_capability(name));
    }
    if let Some(operator) = denied_operator(args) {
        return Err(AppError::new(format!(
            "{operator} runs JavaScript on the server and is not allowed here."
        )));
    }
    let coll = collection(client, args)?;
    let max_time = max_time(args, ctx);
    match name {
        "mongo.find" => find(&coll, args, max_time).await,
        "mongo.count" => count(&coll, args, max_time).await,
        "mongo.distinct" => distinct(&coll, args, max_time).await,
        _ => resolve_ref(&coll, args, max_time).await,
    }
}

/// An argument holding a query document, as JSON: a JSON value as given, JSON text parsed (an empty text is absent).
pub fn json_arg(args: &Json, key: &str) -> AppResult<Option<Json>> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Json::String(text)) => serde_json::from_str(text.trim())
            .map(Some)
            .map_err(|error| AppError::new(format!("{key} must be a JSON document ({error})"))),
        Some(value) => Ok(Some(value.clone())),
    }
}

/// The first server-side JavaScript operator in any query-document argument (text or JSON).
pub fn denied_operator(args: &Json) -> Option<&'static str> {
    find_denied_operator(args).or_else(|| {
        DOCUMENT_ARGS
            .iter()
            .filter_map(|key| json_arg(args, key).ok().flatten())
            .find_map(|value| find_denied_operator(&value))
    })
}

fn text_arg<'a>(args: &'a Json, key: &str) -> AppResult<&'a str> {
    args.get(key)
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| AppError::new(format!("{key} is required")))
}

fn u64_arg(args: &Json, key: &str) -> AppResult<Option<u64>> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| AppError::new(format!("{key} must be a whole number of 0 or more"))),
    }
}

fn bool_arg(args: &Json, key: &str) -> bool {
    args.get(key).and_then(Json::as_bool).unwrap_or(false)
}

fn bson_of(value: Json, key: &str) -> AppResult<Bson> {
    Bson::try_from(value).map_err(|error| AppError::new(format!("{key} is not valid Extended JSON ({error})")))
}

/// A query-document argument as BSON.
pub(super) fn document_arg(args: &Json, key: &str) -> AppResult<Option<Document>> {
    match json_arg(args, key)? {
        None => Ok(None),
        Some(value @ Json::Object(_)) => match bson_of(value, key)? {
            Bson::Document(document) => Ok(Some(document)),
            _ => Err(AppError::new(format!("{key} must be a JSON document"))),
        },
        Some(_) => Err(AppError::new(format!("{key} must be a JSON document"))),
    }
}

fn hint_arg(args: &Json) -> AppResult<Option<Hint>> {
    match args.get("hint") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(name)) if !name.trim_start().starts_with('{') => {
            Ok((!name.trim().is_empty()).then(|| Hint::Name(name.trim().to_owned())))
        }
        Some(_) => Ok(document_arg(args, "hint")?.map(Hint::Keys)),
    }
}

fn collation_arg(args: &Json) -> AppResult<Option<Collation>> {
    document_arg(args, "collation")?
        .map(|document| {
            mongodb::bson::from_document::<Collation>(document).map_err(|error| AppError::new(format!("collation is not valid ({error})")))
        })
        .transpose()
}

fn collection(client: &Client, args: &Json) -> AppResult<Collection<Document>> {
    let database = text_arg(args, "database")?;
    let collection = text_arg(args, "collection")?;
    Ok(client.database(database).collection(collection))
}

/// `maxTimeMS`, else the workspace's timeout, else the default; never above the workspace's timeout or the cap.
fn max_time(args: &Json, ctx: &CallContext) -> Duration {
    let workspace = u64::from(ctx.limits.timeout_secs) * 1000;
    let ceiling = if workspace > 0 { workspace.min(MAX_TIME_MS) } else { MAX_TIME_MS };
    let wanted = args
        .get("maxTimeMS")
        .and_then(Json::as_u64)
        .filter(|ms| *ms > 0)
        .unwrap_or(if workspace > 0 { workspace } else { DEFAULT_MAX_TIME_MS });
    Duration::from_millis(wanted.min(ceiling))
}

fn and(left: Document, right: Document) -> Document {
    match (left.is_empty(), right.is_empty()) {
        (true, _) => right,
        (_, true) => left,
        _ => doc! { "$and": [left, right] },
    }
}

fn relaxed(document: Document) -> Json {
    Bson::Document(document).into_relaxed_extjson()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Whether a sort is `_id` alone, ascending or descending; `Some(descending)` when it is.
fn id_sort(sort: &Document) -> Option<bool> {
    let mut keys = sort.iter();
    let (key, direction) = keys.next()?;
    if key != ID_FIELD || keys.next().is_some() {
        return None;
    }
    let direction = match direction {
        Bson::Int32(n) => f64::from(*n),
        Bson::Int64(n) => *n as f64,
        Bson::Double(n) => *n,
        _ => return None,
    };
    if direction == 1.0 {
        Some(false)
    } else if direction == -1.0 {
        Some(true)
    } else {
        None
    }
}

fn name_json(name: &str) -> String {
    serde_json::to_string(name).unwrap_or_default()
}

async fn find(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let projection = document_arg(args, "projection")?;
    let hint = hint_arg(args)?;
    let collation = collation_arg(args)?;
    let limit = u64_arg(args, "limit")?.unwrap_or(DEFAULT_FIND_LIMIT).clamp(1, MAX_FIND_LIMIT);
    let skip = u64_arg(args, "skip")?.unwrap_or(0);
    // Without a sort (and without a hint that a sort could fight) the documents follow `_id`, so pages are stable.
    let sort = match document_arg(args, "sort")?.filter(|sort| !sort.is_empty()) {
        Some(sort) => Some(sort),
        None if hint.is_none() => Some(doc! { ID_FIELD: 1 }),
        None => None,
    };
    let descending = sort.as_ref().and_then(id_sort);
    let keyset = descending.is_some();
    let query = match json_arg(args, "after")? {
        Some(after) => {
            let Some(descending) = descending else {
                return Err(AppError::new("after pages by _id, so it needs the sort to be _id alone"));
            };
            let operator = if descending { "$lt" } else { "$gt" };
            and(filter.clone(), doc! { ID_FIELD: { operator: bson_of(after, "after")? } })
        }
        None => filter.clone(),
    };
    let mut action = coll
        .find(query.clone())
        .limit(i64::try_from(limit + 1).unwrap_or(i64::MAX))
        .max_time(max_time);
    if let Some(sort) = &sort {
        action = action.sort(sort.clone());
    }
    if skip > 0 {
        action = action.skip(skip);
    }
    if let Some(projection) = &projection {
        action = action.projection(projection.clone());
    }
    if let Some(hint) = hint {
        action = action.hint(hint);
    }
    if let Some(collation) = collation {
        action = action.collation(collation);
    }
    let started = Instant::now();
    let mut documents: Vec<Document> = action.await?.try_collect().await?;
    let elapsed_ms = elapsed_ms(started);
    let has_more = documents.len() as u64 > limit;
    documents.truncate(limit as usize);
    let next_after = documents
        .last()
        .filter(|_| keyset && has_more)
        .and_then(|last| last.get(ID_FIELD))
        .map(|id| id.clone().into_canonical_extjson());
    let mut statement = format!(
        "db.getCollection({}).find({}",
        name_json(coll.name()),
        values::document_text(&query)
    );
    if let Some(projection) = &projection {
        statement.push_str(&format!(", {}", values::document_text(projection)));
    }
    statement.push(')');
    if let Some(sort) = &sort {
        statement.push_str(&format!(".sort({})", values::document_text(sort)));
    }
    if skip > 0 {
        statement.push_str(&format!(".skip({skip})"));
    }
    statement.push_str(&format!(".limit({limit})"));
    Ok(json!({
        "documents": documents.into_iter().map(relaxed).collect::<Vec<_>>(),
        "hasMore": has_more,
        "keyset": keyset,
        "nextAfter": next_after,
        "sort": sort.map(relaxed),
        "elapsedMs": elapsed_ms,
        "statement": statement,
    }))
}

async fn count(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let started = Instant::now();
    if bool_arg(args, "estimated") && filter.is_empty() {
        // A view has no metadata count (CommandNotSupportedOnView): it is counted instead.
        match coll.estimated_document_count().max_time(max_time).await {
            Ok(count) => return Ok(json!({ "count": count, "estimated": true, "elapsedMs": elapsed_ms(started) })),
            Err(error) if is_view_error(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut action = coll.count_documents(filter).max_time(max_time);
    if let Some(limit) = u64_arg(args, "limit")?.filter(|limit| *limit > 0) {
        action = action.limit(limit);
    }
    if let Some(skip) = u64_arg(args, "skip")?.filter(|skip| *skip > 0) {
        action = action.skip(skip);
    }
    if let Some(hint) = hint_arg(args)? {
        action = action.hint(hint);
    }
    if let Some(collation) = collation_arg(args)? {
        action = action.collation(collation);
    }
    let count = action.await?;
    Ok(json!({ "count": count, "estimated": false, "elapsedMs": elapsed_ms(started) }))
}

/// The server's answer to a command a view does not support (code 166, `CommandNotSupportedOnView`).
fn is_view_error(error: &mongodb::error::Error) -> bool {
    matches!(error.kind.as_ref(), mongodb::error::ErrorKind::Command(command) if command.code == 166)
}

/// A field path for `distinct`: dotted names, no `$` at the start of a part (that would be an expression or a variable).
fn field_path(args: &Json) -> AppResult<&str> {
    let field = text_arg(args, "field")?;
    if field.split('.').any(|part| part.is_empty() || part.starts_with('$')) || field.contains('\0') {
        return Err(AppError::new(format!("'{field}' is not a field path")));
    }
    Ok(field)
}

/// Distinct values through an aggregation, so the result is capped: array fields give their elements (like the
/// `distinct` command), missing and null values are left out.
async fn distinct(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let field = field_path(args)?;
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let limit = u64_arg(args, "limit")?
        .unwrap_or(DEFAULT_DISTINCT_LIMIT)
        .clamp(1, MAX_DISTINCT_LIMIT);
    let pipeline = vec![
        doc! { "$match": filter },
        doc! { "$project": { "_id": 0, "v": format!("${field}") } },
        doc! { "$unwind": "$v" },
        doc! { "$group": { "_id": "$v" } },
        doc! { "$sort": { "_id": 1 } },
        doc! { "$limit": i64::try_from(limit + 1).unwrap_or(i64::MAX) },
    ];
    let mut action = coll.aggregate(pipeline).max_time(max_time);
    if let Some(collation) = collation_arg(args)? {
        action = action.collation(collation);
    }
    let started = Instant::now();
    let groups: Vec<Document> = action.await?.try_collect().await?;
    let elapsed_ms = elapsed_ms(started);
    let truncated = groups.len() as u64 > limit;
    let values: Vec<Json> = groups
        .into_iter()
        .take(limit as usize)
        .filter_map(|mut group| group.remove(ID_FIELD))
        .map(Bson::into_relaxed_extjson)
        .collect();
    Ok(json!({ "values": values, "truncated": truncated, "elapsedMs": elapsed_ms }))
}

async fn resolve_ref(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let id = match args.get("id") {
        Some(id) => bson_of(id.clone(), "id")?,
        None => return Err(AppError::new("id is required")),
    };
    let mut action = coll.find_one(doc! { ID_FIELD: id }).max_time(max_time);
    if let Some(projection) = document_arg(args, "projection")? {
        action = action.projection(projection);
    }
    let document = action.await?;
    Ok(json!({ "document": document.map(relaxed) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Limits;

    fn ctx(timeout_secs: u32) -> CallContext {
        CallContext::new(Limits {
            timeout_secs,
            ..Limits::default()
        })
    }

    #[test]
    fn time_bounds_follow_the_workspace() {
        assert_eq!(max_time(&json!({}), &ctx(0)), Duration::from_millis(DEFAULT_MAX_TIME_MS));
        assert_eq!(max_time(&json!({ "maxTimeMS": 500 }), &ctx(0)), Duration::from_millis(500));
        assert_eq!(max_time(&json!({}), &ctx(5)), Duration::from_secs(5));
        assert_eq!(max_time(&json!({ "maxTimeMS": 60_000 }), &ctx(5)), Duration::from_secs(5));
        assert_eq!(
            max_time(&json!({ "maxTimeMS": 10_000_000 }), &ctx(0)),
            Duration::from_millis(MAX_TIME_MS)
        );
    }

    #[test]
    fn query_documents_parse_from_json_or_text() {
        let args = json!({
            "filter": "{ \"_id\": { \"$oid\": \"65f1a2b3c4d5e6f708192a3b\" } }",
            "sort": { "n": { "$numberInt": "-1" } },
            "projection": "",
        });
        let filter = document_arg(&args, "filter").unwrap().unwrap();
        assert!(matches!(filter.get(ID_FIELD), Some(Bson::ObjectId(_))));
        assert_eq!(document_arg(&args, "sort").unwrap().unwrap().get("n"), Some(&Bson::Int32(-1)));
        assert!(document_arg(&args, "projection").unwrap().is_none());
        assert!(document_arg(&json!({ "filter": [1] }), "filter").is_err());
        assert!(document_arg(&json!({ "filter": "{ nope" }), "filter").is_err());
    }

    #[test]
    fn server_javascript_is_found_in_json_and_text() {
        assert_eq!(denied_operator(&json!({ "filter": { "$where": "true" } })), Some("$where"));
        assert_eq!(denied_operator(&json!({ "filter": "{ \"$where\": \"true\" }" })), Some("$where"));
        assert_eq!(
            denied_operator(&json!({ "projection": { "x": { "$function": { "body": "f", "args": [], "lang": "js" } } } })),
            Some("$function")
        );
        assert_eq!(denied_operator(&json!({ "filter": { "a": 1 }, "sort": { "a": 1 } })), None);
    }

    #[test]
    fn keyset_paging_needs_an_id_sort() {
        assert_eq!(id_sort(&doc! { "_id": 1 }), Some(false));
        assert_eq!(id_sort(&doc! { "_id": -1_i64 }), Some(true));
        assert_eq!(id_sort(&doc! { "_id": 1, "n": 1 }), None);
        assert_eq!(id_sort(&doc! { "n": 1 }), None);
        assert_eq!(id_sort(&doc! { "_id": "text" }), None);
    }

    #[test]
    fn hints_take_a_name_or_keys() {
        assert!(matches!(hint_arg(&json!({ "hint": "name_1" })).unwrap(), Some(Hint::Name(name)) if name == "name_1"));
        assert!(matches!(hint_arg(&json!({ "hint": { "name": 1 } })).unwrap(), Some(Hint::Keys(_))));
        assert!(hint_arg(&json!({})).unwrap().is_none());
    }

    #[test]
    fn distinct_fields_are_plain_paths() {
        assert_eq!(field_path(&json!({ "field": "meta.tags" })).unwrap(), "meta.tags");
        for bad in ["", "$$ROOT", "a.$b", "a..b"] {
            assert!(field_path(&json!({ "field": bad })).is_err(), "{bad}");
        }
    }
}
