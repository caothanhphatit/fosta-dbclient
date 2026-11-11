//! Browsing and editing documents.
//!
//! Browse: structured conditions become a query document with values parsed to the field's stored type (sampled from
//! one document that has the field), raw filters are MongoDB JSON (Extended JSON allowed; the gateway already refused
//! `$where` / `$function` / `$accumulator`). Pages are sorted by `_id` unless the user sorts, so offsets are stable and
//! a page sorted by `_id` alone gives a keyset cursor.
//!
//! Apply: every update and delete is checked against what the user read. The current documents of every key are read
//! first; when any change no longer matches (the document is gone, or an edited field changed), nothing is written and
//! the error is `CONFLICT` with those documents. Then the changes are written in order, each guarded by the key and the
//! values just read, so a write that races the check matches nothing and is a conflict too.
//!
//! Columns of an update are dotted paths (`address.city`, `tags.1`): the update is `$set` / `$unset` by path, so a nested
//! edit leaves the rest of its top-level field alone, and only the edited paths are compared. The document views send
//! exact values ([`values::exact`]: Extended JSON written as given, originals compared as they were loaded) and removed
//! fields ([`values::unset`]); the grid sends typed values coerced to the stored type.
//!
//! - On a replica set or a sharded cluster the whole apply runs in one transaction: any conflict or error aborts it.
//! - On a standalone server (no transactions) the writes already made are compensated when a later one conflicts or
//!   fails: updated documents are replaced by the copies read before, deleted ones are inserted again, inserted ones
//!   are removed. The error then carries `"compensated": true` (`false` when an undo step failed as well). A write by
//!   someone else to the same documents between the apply and its compensation would be overwritten, and documents of
//!   an insert batch cut off by a lost connection (not a write error) are not removed.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, oid::ObjectId, Bson, Document},
    error::{Error as MongoError, ErrorKind},
    Client, ClientSession, Collection,
};

use super::{
    catalog,
    values::{self, coerce, fields_of, from_value, get_path, key_to_bson, matches_original, set_path, to_value, ID_FIELD},
};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        value::{parse_input, parse_timestamptz, unhex},
        ApplyResult, BrowseRequest, CallContext, Field, Filter, FilterCond, FilterOp, NodeKind, Page, RowChange, RowChangeSet, RowCheck,
        SortKey, Total, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

/// Pages are capped so one request cannot pull a whole collection.
const MAX_PAGE_SIZE: u64 = 5000;
/// Below this many documents (by the collection's metadata) a filtered total is counted exactly.
const EXACT_COUNT_BELOW: u64 = 50_000;
/// Keys per `$in` when reading the documents an apply touches.
const KEYS_PER_READ: usize = 1000;

fn database_of(object: &crate::contract::ObjectRef) -> AppResult<&str> {
    object
        .database
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::new("The collection has no database"))
}

fn collection(client: &Client, object: &crate::contract::ObjectRef) -> AppResult<Collection<Document>> {
    Ok(client.database(database_of(object)?).collection::<Document>(&object.name))
}

// --- Filters ----------------------------------------------------------------------------------------------------------

/// Escapes regex metacharacters so text matches literally.
pub fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A SQL `LIKE` pattern as an anchored regex: `%` any run, `_` one character, `\` escapes the next one.
pub fn like_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push_str(&escape_regex(&next.to_string()));
                }
            }
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            other => out.push_str(&escape_regex(&other.to_string())),
        }
    }
    out.push('$');
    out
}

/// A typed filter value for a field whose stored values look like `sample` (`None` when no document has it): numbers
/// for numeric fields, ObjectIds for ObjectId fields (and 24-hex text on `_id`), dates for dates, text otherwise.
pub fn filter_value(column: &str, sample: Option<&Bson>, text: &str) -> AppResult<Bson> {
    let sample = match sample {
        Some(Bson::Array(items)) => items.iter().find(|item| !matches!(item, Bson::Null)),
        other => other,
    };
    let trimmed = text.trim();
    let refuse = |what: &str| AppError::new(format!("'{text}' is not {what}; {column} holds {what}s."));
    Ok(match sample {
        Some(Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) => match trimmed.parse::<i64>() {
            Ok(number) => Bson::Int64(number),
            Err(_) => Bson::Double(trimmed.parse::<f64>().map_err(|_| refuse("a number"))?),
        },
        Some(Bson::Decimal128(_)) => match trimmed.parse() {
            Ok(number) if crate::contract::value::is_decimal_text(trimmed) => Bson::Decimal128(number),
            _ => return Err(refuse("a number")),
        },
        Some(Bson::Boolean(_)) => match parse_input(&ValueKind::Bool, trimmed) {
            Ok(Value::Bool(flag)) => Bson::Boolean(flag),
            _ => return Err(refuse("a boolean")),
        },
        Some(Bson::DateTime(_)) => Bson::DateTime(mongodb::bson::DateTime::from_millis(
            parse_timestamptz(trimmed).ok_or_else(|| refuse("a date"))?.timestamp_millis(),
        )),
        Some(Bson::ObjectId(_)) => Bson::ObjectId(ObjectId::parse_str(trimmed).map_err(|_| refuse("an ObjectId"))?),
        Some(binary @ Bson::Binary(_)) => match values::kind_of(binary) {
            ValueKind::Uuid => from_value(&parse_input(&ValueKind::Uuid, trimmed).map_err(|_| refuse("a UUID"))?)?,
            _ => from_value(&Value::Bytes(unhex(trimmed, false).ok_or_else(|| refuse("a hex byte string"))?))?,
        },
        None if column == ID_FIELD => key_to_bson(column, &Value::Text(trimmed.to_owned()))?,
        _ => Bson::String(text.to_owned()),
    })
}

/// A regex match on a field: `$regex` on string fields; on other types the field's text form is matched (`$convert`
/// in `$expr`). Negated matches leave out documents without the field, as `NOT LIKE` leaves out NULL.
fn regex_condition(column: &str, pattern: &str, options: &str, negate: bool, string_field: bool) -> Document {
    if string_field {
        return if negate {
            doc! { column: { "$not": values::regex(pattern, options), "$ne": Bson::Null } }
        } else {
            doc! { column: { "$regex": pattern, "$options": options } }
        };
    }
    let input = doc! { "$convert": { "input": format!("${column}"), "to": "string", "onError": "", "onNull": Bson::Null } };
    let matched = doc! { "$regexMatch": { "input": input, "regex": pattern, "options": options } };
    if negate {
        doc! { "$and": [ { column: { "$ne": Bson::Null } }, { "$expr": { "$not": [matched] } } ] }
    } else {
        doc! { "$expr": matched }
    }
}

/// One structured condition as a query document. `sample` is a stored value of the field (its type decides how the
/// typed text is parsed).
pub fn condition(cond: &FilterCond, sample: Option<&Bson>) -> AppResult<Document> {
    cond.validate()?;
    let column = cond.column.as_str();
    let value = |index: usize| filter_value(column, sample, &cond.values[index]);
    let all = || {
        cond.values
            .iter()
            .map(|text| filter_value(column, sample, text))
            .collect::<AppResult<Vec<Bson>>>()
    };
    let string_field = match sample {
        Some(Bson::Array(items)) => items.iter().all(|item| matches!(item, Bson::String(_) | Bson::Null)),
        Some(other) => matches!(other, Bson::String(_) | Bson::Symbol(_)),
        None => true,
    };
    let text = || cond.values.first().map(String::as_str).unwrap_or_default();
    Ok(match cond.op {
        FilterOp::Eq => doc! { column: { "$eq": value(0)? } },
        // Like SQL's <>, documents without the field are not "different".
        FilterOp::Ne => doc! { column: { "$nin": [value(0)?, Bson::Null] } },
        FilterOp::Lt => doc! { column: { "$lt": value(0)? } },
        FilterOp::Le => doc! { column: { "$lte": value(0)? } },
        FilterOp::Gt => doc! { column: { "$gt": value(0)? } },
        FilterOp::Ge => doc! { column: { "$gte": value(0)? } },
        FilterOp::Contains => regex_condition(column, &escape_regex(text()), "i", false, string_field),
        FilterOp::NotContains => regex_condition(column, &escape_regex(text()), "i", true, string_field),
        FilterOp::StartsWith => regex_condition(column, &format!("^{}", escape_regex(text())), "i", false, string_field),
        FilterOp::EndsWith => regex_condition(column, &format!("{}$", escape_regex(text())), "i", false, string_field),
        FilterOp::Like => regex_condition(column, &like_regex(text()), "s", false, string_field),
        FilterOp::NotLike => regex_condition(column, &like_regex(text()), "s", true, string_field),
        FilterOp::In => doc! { column: { "$in": all()? } },
        FilterOp::NotIn => {
            let mut values = all()?;
            values.push(Bson::Null);
            doc! { column: { "$nin": values } }
        }
        FilterOp::Between => doc! { column: { "$gte": value(0)?, "$lte": value(1)? } },
        FilterOp::IsNull => doc! { column: { "$eq": Bson::Null } },
        FilterOp::IsNotNull => doc! { column: { "$ne": Bson::Null } },
    })
}

/// Conditions joined with `$and`, or `$or` when any may match.
pub fn combine(mut parts: Vec<Document>, match_any: bool) -> Document {
    match parts.len() {
        0 => Document::new(),
        1 => parts.remove(0),
        _ if match_any => doc! { "$or": parts },
        _ => doc! { "$and": parts },
    }
}

/// A stored value of a field, to type the filter's values.
async fn field_sample(coll: &Collection<Document>, column: &str) -> AppResult<Option<Bson>> {
    let found = coll
        .find_one(doc! { column: { "$exists": true, "$ne": Bson::Null } })
        .projection(doc! { column: 1 })
        .await?;
    Ok(found.and_then(|document| get_path(&document, column).cloned()))
}

async fn conditions_query(coll: &Collection<Document>, conditions: &[FilterCond], match_any: bool) -> AppResult<Document> {
    let mut samples: HashMap<&str, Option<Bson>> = HashMap::new();
    for cond in conditions {
        if !samples.contains_key(cond.column.as_str()) {
            samples.insert(&cond.column, field_sample(coll, &cond.column).await?);
        }
    }
    let parts = conditions
        .iter()
        .map(|cond| condition(cond, samples.get(cond.column.as_str()).and_then(Option::as_ref)))
        .collect::<AppResult<Vec<_>>>()?;
    Ok(combine(parts, match_any))
}

pub fn raw_filter(text: &str) -> AppResult<Document> {
    if text.trim().is_empty() {
        return Ok(Document::new());
    }
    values::parse_document(text).map_err(|error| {
        AppError::new(format!(
            "Filter must be a JSON query document, e.g. {{ \"status\": \"ACTIVE\" }}. ({error})"
        ))
    })
}

fn and(left: Document, right: Document) -> Document {
    match (left.is_empty(), right.is_empty()) {
        (true, _) => right,
        (_, true) => left,
        _ => doc! { "$and": [left, right] },
    }
}

// --- Browse -------------------------------------------------------------------------------------------------------------

pub async fn browse(client: &Client, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
    let coll = collection(client, &request.object)?;
    let view = request.object.kind == NodeKind::VIEW;
    let max_time = (ctx.limits.timeout_secs > 0).then(|| Duration::from_secs(u64::from(ctx.limits.timeout_secs)));
    let filter = match &request.filter {
        Filter::None => Document::new(),
        Filter::Conditions { conditions, match_any } => conditions_query(&coll, conditions, *match_any).await?,
        Filter::Raw { text, .. } => raw_filter(text)?,
    };
    // Unsorted pages follow `_id` (always indexed) so offsets are stable; views keep their pipeline's order.
    let sort: Vec<SortKey> = if request.sort.is_empty() && !view {
        vec![SortKey::asc(ID_FIELD)]
    } else {
        request.sort.clone()
    };
    let keyset = sort.len() == 1 && sort[0].column == ID_FIELD;
    let after = request.after.as_ref().and_then(|after| after.first()).filter(|_| keyset);
    let query = match after {
        Some(last) => {
            let operator = if sort[0].descending { "$lt" } else { "$gt" };
            and(filter.clone(), doc! { ID_FIELD: { operator: key_to_bson(ID_FIELD, last)? } })
        }
        None => filter.clone(),
    };
    let limit = request.limit.clamp(1, MAX_PAGE_SIZE);
    let skip = if after.is_some() { 0 } else { request.offset };
    let sort_doc: Document = sort
        .iter()
        .map(|key| (key.column.clone(), Bson::Int32(if key.descending { -1 } else { 1 })))
        .collect();
    let mut find = coll.find(query.clone()).limit(i64::try_from(limit + 1).unwrap_or(i64::MAX));
    if !sort_doc.is_empty() {
        find = find.sort(sort_doc.clone());
    }
    if skip > 0 {
        find = find.skip(skip);
    }
    if let Some(columns) = request.columns.as_ref().filter(|columns| !columns.is_empty()) {
        find = find.projection(columns.iter().map(|column| (column.clone(), Bson::Int32(1))).collect::<Document>());
    }
    if let Some(max_time) = max_time {
        find = find.max_time(max_time);
    }
    let started = Instant::now();
    let mut documents: Vec<Document> = find.await?.try_collect().await?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let has_more = documents.len() as u64 > limit;
    documents.truncate(limit as usize);
    let columns = if documents.is_empty() {
        // An empty page still shows the collection's fields.
        let db = client.database(database_of(&request.object)?);
        catalog::sampled_columns(&catalog::sample(&db, &request.object.name).await.unwrap_or_default())
    } else {
        values::columns_of(&documents)
    };
    let next_keyset = documents
        .last()
        .filter(|_| keyset && has_more)
        .and_then(|last| last.get(ID_FIELD))
        .map(|id| vec![to_value(id)]);
    let total = total(&coll, &filter, request, view, max_time).await?;
    let mut statement = format!(
        "db.getCollection({}).find({})",
        serde_json::to_string(&request.object.name).unwrap_or_default(),
        values::document_text(&query)
    );
    if !sort_doc.is_empty() {
        statement.push_str(&format!(".sort({})", values::document_text(&sort_doc)));
    }
    if skip > 0 {
        statement.push_str(&format!(".skip({skip})"));
    }
    statement.push_str(&format!(".limit({limit})"));
    Ok(Page {
        rows: values::rows_of(&documents, &columns),
        columns,
        has_more,
        key_columns: vec![ID_FIELD.to_owned()],
        next_keyset,
        total,
        statement: Some(statement),
        elapsed_ms,
        row_versions: None,
    })
}

/// The collection's metadata count when unfiltered, an exact count when asked for or when the collection is small.
async fn total(
    coll: &Collection<Document>,
    filter: &Document,
    request: &BrowseRequest,
    view: bool,
    max_time: Option<Duration>,
) -> AppResult<Option<Total>> {
    let count = |filter: Document| {
        let mut count = coll.count_documents(filter);
        if let Some(max_time) = max_time {
            count = count.max_time(max_time);
        }
        count
    };
    if request.exact_total || (view && request.estimate_total) {
        return Ok(Some(Total {
            value: count(filter.clone()).await?,
            exact: true,
        }));
    }
    if !request.estimate_total {
        return Ok(None);
    }
    let estimate = coll.estimated_document_count().await.ok();
    if filter.is_empty() {
        return Ok(estimate.map(|value| Total { value, exact: false }));
    }
    match estimate {
        Some(estimate) if estimate < EXACT_COUNT_BELOW => Ok(Some(Total {
            value: count(filter.clone()).await?,
            exact: true,
        })),
        _ => Ok(None),
    }
}

// --- Apply --------------------------------------------------------------------------------------------------------------

/// What undoes one applied write on a standalone server.
enum Undo {
    /// An updated document: put back the copy read before.
    Restore(Document),
    /// A deleted document: insert it again.
    Reinsert(Document),
    /// Inserted documents: remove them by `_id`.
    Remove(Vec<Bson>),
}

/// How a run of writes ended.
enum Stop {
    /// A guarded write matched nothing (someone changed or deleted the document after the check).
    Conflict(usize),
    Failed(AppError),
}

impl From<AppError> for Stop {
    fn from(error: AppError) -> Self {
        Stop::Failed(error)
    }
}

impl From<MongoError> for Stop {
    fn from(error: MongoError) -> Self {
        Stop::Failed(error.into())
    }
}

/// Runs a driver action in the apply's session when there is one.
macro_rules! in_session {
    ($session:expr, $action:expr) => {
        match $session {
            Some(session) => $action.session(session).await,
            None => $action.await,
        }
    };
}

struct Applier<'a> {
    coll: Collection<Document>,
    set: &'a RowChangeSet,
    session: Option<ClientSession>,
    undo: Vec<Undo>,
    result: ApplyResult,
}

/// The query that finds a change's document by its key, as the user saw it.
fn key_query(key: &[Field]) -> AppResult<Document> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the document: the change has no key"));
    }
    key.iter()
        .map(|field| Ok((field.column.clone(), key_to_bson(&field.column, &field.value)?)))
        .collect()
}

/// Whether the document still matches what the change was based on.
fn still_matches(change: &RowChange, current: Option<&Document>) -> bool {
    let Some(current) = current else { return false };
    match change.check() {
        RowCheck::KeyOnly | RowCheck::Version(_) => true,
        RowCheck::Original(fields) => fields
            .iter()
            .all(|field| matches_original(get_path(current, &field.column), &field.value)),
    }
}

/// The filter of a guarded write: the key and the checked fields with the exact values read before the write.
fn guard(change: &RowChange, current: &Document) -> Document {
    let mut filter = Document::new();
    for field in change.key() {
        let stored = get_path(current, &field.column).cloned().unwrap_or(Bson::Null);
        filter.insert(field.column.clone(), doc! { "$eq": stored });
    }
    if let RowCheck::Original(fields) = change.check() {
        for field in fields {
            if !filter.contains_key(&field.column) {
                let stored = get_path(current, &field.column).cloned().unwrap_or(Bson::Null);
                filter.insert(field.column.clone(), doc! { "$eq": stored });
            }
        }
    }
    filter
}

/// The `$set` of an update: each value coerced to the type the field has now (or had when read).
#[cfg(test)]
fn set_document(values: &[Field], original: &[Field], current: &Document) -> AppResult<Document> {
    let update = update_document(values, original, current)?;
    Ok(update.get_document("$set").cloned().unwrap_or_default())
}

/// Whether one dotted path is the other or lies inside it (`a` and `a.b`): one update cannot touch both.
pub(super) fn overlaps(left: &str, right: &str) -> bool {
    let inside = |outer: &str, inner: &str| inner.len() > outer.len() && inner.starts_with(outer) && inner.as_bytes()[outer.len()] == b'.';
    left == right || inside(left, right) || inside(right, left)
}

/// A field path an update may name: dotted parts, none empty or starting with `$` (positional operators and
/// expressions are not field edits).
fn check_path(path: &str) -> AppResult<()> {
    if path.is_empty() || path.split('.').any(|part| part.is_empty() || part.starts_with('$')) || path.contains('\0') {
        return Err(AppError::new(format!("'{path}' is not a field path that can be edited")));
    }
    Ok(())
}

/// The update of a change, by dotted path so a nested edit leaves the rest of its top-level field alone: `$set` for
/// new values, `$unset` for removed fields ([`values::unset`]). Values are coerced to the type the field has now (or
/// had when read); exact values ([`values::exact`]) are written as given, a document or array keeping the stored types
/// of its numbers (relaxed JSON cannot tell an int from a long). Paths that overlap are refused.
fn update_document(values: &[Field], original: &[Field], current: &Document) -> AppResult<Document> {
    let mut set = Document::new();
    let mut unset = Document::new();
    for (index, field) in values.iter().enumerate() {
        check_path(&field.column)?;
        if field.column == ID_FIELD || field.column.starts_with("_id.") {
            return Err(AppError::new(
                "_id cannot be changed; insert a copy with the new _id and delete this document",
            ));
        }
        if let Some(other) = values[..index].iter().find(|other| overlaps(&other.column, &field.column)) {
            return Err(AppError::new(format!(
                "One change cannot edit both {} and {}; save one of them first",
                other.column, field.column
            )));
        }
        if values::is_unset(&field.value) {
            unset.insert(field.column.clone(), "");
            continue;
        }
        let template = match get_path(current, &field.column) {
            Some(stored) => Some(stored.clone()),
            None => original
                .iter()
                .find(|before| before.column == field.column)
                .map(|before| from_value(&before.value))
                .transpose()?,
        };
        let value = match values::exact_bson(&field.value) {
            Some(bson) => match (bson?, template.as_ref()) {
                (bson @ (Bson::Document(_) | Bson::Array(_)), Some(template @ (Bson::Document(_) | Bson::Array(_)))) => {
                    values::match_types(bson, template)
                }
                (bson, _) => bson,
            },
            None => coerce(&field.column, &field.value, template.as_ref())?,
        };
        set.insert(field.column.clone(), value);
    }
    let mut update = Document::new();
    if !set.is_empty() {
        update.insert("$set", set);
    }
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }
    Ok(update)
}

/// The document as it is now for the conflict dialog: its top-level fields, plus every dotted path the change touched
/// (in the form the change's original used, so "When opened" and "Now" compare like for like).
fn current_fields(change: &RowChange, document: &Document) -> Vec<Field> {
    let mut fields = fields_of(document);
    let (values, original): (&[Field], &[Field]) = match change {
        RowChange::Update { values, original, .. } => (values, original),
        RowChange::Delete { original, .. } => (&[], original),
        RowChange::Insert { .. } => (&[], &[]),
    };
    for column in values.iter().chain(original).map(|field| field.column.as_str()) {
        let exact = original
            .iter()
            .any(|field| field.column == column && values::is_exact(&field.value));
        if fields.iter().any(|field| field.column == column) && !exact {
            continue;
        }
        let stored = get_path(document, column);
        let value = if exact {
            values::exact(values::relaxed_or_null(stored).to_string())
        } else {
            stored.map(to_value).unwrap_or(Value::Null)
        };
        match fields.iter_mut().find(|field| field.column == column) {
            Some(field) => field.value = value,
            None => fields.push(Field::new(column, value)),
        }
    }
    fields
}

/// A new document from an insert's fields (dotted columns nest), with an `_id` so it can be removed again.
fn insert_document(values: &[Field]) -> AppResult<Document> {
    let mut document = Document::new();
    for field in values {
        let value = match values::exact_bson(&field.value) {
            Some(bson) => bson?,
            None => from_value(&field.value)?,
        };
        set_path(&mut document, &field.column, value);
    }
    if !document.contains_key(ID_FIELD) {
        let mut with_id = doc! { ID_FIELD: ObjectId::new() };
        with_id.extend(document);
        document = with_id;
    }
    Ok(document)
}

/// The first failed position of an ordered `insert_many`, when the driver reports it.
fn first_failed_insert(error: &MongoError) -> Option<usize> {
    match error.kind.as_ref() {
        ErrorKind::InsertMany(failure) => match &failure.write_errors {
            Some(errors) => errors.iter().map(|error| error.index).min(),
            // Only the write concern failed: every document was written.
            None => Some(usize::MAX),
        },
        _ => None,
    }
}

impl Applier<'_> {
    /// The current document of every update and delete, by position (`None` for inserts and missing documents).
    async fn read_current(&mut self) -> AppResult<Vec<Option<Document>>> {
        let set = self.set;
        let changes = &set.changes;
        let mut current: Vec<Option<Document>> = vec![None; changes.len()];
        let by_id = changes
            .iter()
            .all(|change| matches!(change, RowChange::Insert { .. }) || matches!(change.key(), [field] if field.column == ID_FIELD));
        if !by_id {
            for (index, change) in changes.iter().enumerate() {
                if !matches!(change, RowChange::Insert { .. }) {
                    let query = key_query(change.key())?;
                    current[index] = in_session!(self.session.as_mut(), self.coll.find_one(query))?;
                }
            }
            return Ok(current);
        }
        let wanted: Vec<(usize, &Value)> = changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| change.key().first().map(|field| (index, &field.value)))
            .collect();
        let mut found: Vec<Document> = Vec::new();
        for chunk in wanted.chunks(KEYS_PER_READ) {
            let ids = chunk
                .iter()
                .map(|(_, value)| key_to_bson(ID_FIELD, value))
                .collect::<AppResult<Vec<_>>>()?;
            let query = doc! { ID_FIELD: { "$in": ids } };
            let documents: Vec<Document> = match self.session.as_mut() {
                Some(session) => {
                    let mut cursor = self.coll.find(query).session(&mut *session).await?;
                    cursor.stream(session).try_collect().await?
                }
                None => self.coll.find(query).await?.try_collect().await?,
            };
            found.extend(documents);
        }
        let mut by_text: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, document) in found.iter().enumerate() {
            let text = document
                .get(ID_FIELD)
                .map(to_value)
                .and_then(|value| value.display())
                .unwrap_or_default();
            by_text.entry(text).or_default().push(position);
        }
        for (index, value) in wanted {
            // An exact `_id` (Extended JSON from the document views) is looked up by the text of the value it holds.
            let text = if values::is_exact(value) {
                key_to_bson(ID_FIELD, value).ok().and_then(|id| to_value(&id).display())
            } else {
                value.display()
            }
            .unwrap_or_default();
            current[index] = by_text.get(&text).and_then(|positions| {
                positions
                    .iter()
                    .map(|position| &found[*position])
                    .find(|document| matches_original(document.get(ID_FIELD), value))
                    .cloned()
            });
        }
        Ok(current)
    }

    /// Writes the changes in order; stops at the first guarded write that matches nothing or fails.
    async fn write(&mut self, current: &[Option<Document>]) -> Result<(), Stop> {
        let set = self.set;
        let changes = &set.changes;
        let mut index = 0;
        while index < changes.len() {
            match &changes[index] {
                RowChange::Insert { .. } => {
                    let mut documents = Vec::new();
                    while let Some(RowChange::Insert { values }) = changes.get(index) {
                        documents.push(insert_document(values)?);
                        index += 1;
                    }
                    let ids: Vec<Bson> = documents.iter().filter_map(|document| document.get(ID_FIELD).cloned()).collect();
                    match in_session!(self.session.as_mut(), self.coll.insert_many(documents).ordered(true)) {
                        Ok(inserted) => {
                            self.result.inserted += inserted.inserted_ids.len() as u64;
                            self.undo.push(Undo::Remove(ids));
                        }
                        Err(error) => {
                            if let Some(failed) = first_failed_insert(&error) {
                                self.undo.push(Undo::Remove(ids.into_iter().take(failed).collect()));
                            }
                            return Err(error.into());
                        }
                    }
                    continue;
                }
                change @ RowChange::Update { values, original, .. } => {
                    let document = current[index].as_ref().ok_or(Stop::Conflict(index))?;
                    if !values.is_empty() {
                        let update = update_document(values, original, document)?;
                        let outcome = in_session!(self.session.as_mut(), self.coll.update_one(guard(change, document), update))?;
                        if outcome.matched_count == 0 {
                            return Err(Stop::Conflict(index));
                        }
                        self.result.updated += 1;
                        self.undo.push(Undo::Restore(document.clone()));
                    }
                }
                change @ RowChange::Delete { .. } => {
                    let document = current[index].as_ref().ok_or(Stop::Conflict(index))?;
                    let outcome = in_session!(self.session.as_mut(), self.coll.delete_one(guard(change, document)))?;
                    if outcome.deleted_count == 0 {
                        return Err(Stop::Conflict(index));
                    }
                    self.result.deleted += 1;
                    self.undo.push(Undo::Reinsert(document.clone()));
                }
            }
            index += 1;
        }
        Ok(())
    }

    /// Undoes the writes made so far, newest first. Returns false when an undo step failed.
    async fn compensate(&mut self) -> bool {
        let mut complete = true;
        while let Some(step) = self.undo.pop() {
            let undone = match step {
                Undo::Restore(document) => {
                    let id = document.get(ID_FIELD).cloned().unwrap_or(Bson::Null);
                    self.coll.replace_one(doc! { ID_FIELD: id }, document).await.map(|_| ())
                }
                Undo::Reinsert(document) => self.coll.insert_one(document).await.map(|_| ()),
                Undo::Remove(ids) if ids.is_empty() => Ok(()),
                Undo::Remove(ids) => self.coll.delete_many(doc! { ID_FIELD: { "$in": ids } }).await.map(|_| ()),
            };
            if let Err(error) = undone {
                tracing::warn!(%error, "could not undo a write of a failed apply");
                complete = false;
            }
        }
        complete
    }

    /// The conflict error for change `index` with its document as it is now.
    async fn race_conflict(&self, index: usize) -> AppError {
        let change = &self.set.changes[index];
        let now = match key_query(change.key()) {
            Ok(query) => self.coll.find_one(query).await.ok().flatten(),
            Err(_) => None,
        };
        conflict_error(
            &self.set.object,
            vec![RowConflict::new(index, change, now.as_ref().map(|now| current_fields(change, now)))],
        )
    }
}

pub async fn apply(client: &Client, transactions: bool, set: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
    if set.changes.is_empty() {
        return Ok(ApplyResult::default());
    }
    let mut applier = Applier {
        coll: collection(client, &set.object)?,
        set,
        session: None,
        undo: Vec::new(),
        result: ApplyResult::default(),
    };
    if transactions {
        let mut session = client.start_session().await?;
        session.start_transaction().await?;
        applier.session = Some(session);
    }
    let abort = |applier: &mut Applier<'_>| {
        let session = applier.session.take();
        async move {
            if let Some(mut session) = session {
                let _ = session.abort_transaction().await;
            }
        }
    };
    let current = match applier.read_current().await {
        Ok(current) => current,
        Err(error) => {
            abort(&mut applier).await;
            return Err(error);
        }
    };
    let conflicts: Vec<RowConflict> = set
        .changes
        .iter()
        .enumerate()
        .filter(|(index, change)| !matches!(change, RowChange::Insert { .. }) && !still_matches(change, current[*index].as_ref()))
        .map(|(index, change)| RowConflict::new(index, change, current[index].as_ref().map(|now| current_fields(change, now))))
        .collect();
    if !conflicts.is_empty() {
        abort(&mut applier).await;
        return Err(conflict_error(&set.object, conflicts));
    }
    let stopped = applier.write(&current).await.err();
    if transactions {
        match stopped {
            None => {
                if let Some(mut session) = applier.session.take() {
                    session.commit_transaction().await?;
                }
                Ok(applier.result)
            }
            Some(stop) => {
                abort(&mut applier).await;
                Err(match stop {
                    Stop::Conflict(index) => applier.race_conflict(index).await,
                    Stop::Failed(error) => error,
                })
            }
        }
    } else {
        match stopped {
            None => Ok(applier.result),
            Some(stop) => {
                let wrote = !applier.undo.is_empty();
                let compensated = applier.compensate().await;
                let error = match stop {
                    Stop::Conflict(index) => applier.race_conflict(index).await,
                    Stop::Failed(error) => error,
                };
                Err(if wrote { error.with("compensated", compensated) } else { error })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(column: &str, op: FilterOp, values: &[&str]) -> FilterCond {
        FilterCond::new(column, op, values)
    }

    #[test]
    fn regex_and_like_patterns_escape_specials() {
        assert_eq!(escape_regex("a.b*(c)"), r"a\.b\*\(c\)");
        assert_eq!(like_regex("item 00_1"), "^item 00.1$");
        assert_eq!(like_regex("50\\%%"), "^50%.*$");
        assert_eq!(like_regex("a.c"), r"^a\.c$");
    }

    #[test]
    fn filter_values_follow_the_stored_type() {
        assert_eq!(filter_value("n", Some(&Bson::Int32(1)), "5").unwrap(), Bson::Int64(5));
        assert_eq!(filter_value("n", Some(&Bson::Double(1.0)), "5.5").unwrap(), Bson::Double(5.5));
        assert!(filter_value("n", Some(&Bson::Int32(1)), "five").is_err());
        assert_eq!(
            filter_value("s", Some(&Bson::String("x".into())), "5").unwrap(),
            Bson::String("5".into())
        );
        let id = ObjectId::new();
        assert_eq!(
            filter_value("_id", Some(&Bson::ObjectId(id)), &id.to_hex()).unwrap(),
            Bson::ObjectId(id)
        );
        assert_eq!(filter_value("_id", None, &id.to_hex()).unwrap(), Bson::ObjectId(id));
        assert!(matches!(
            filter_value("at", Some(&Bson::DateTime(mongodb::bson::DateTime::now())), "2026-01-02").unwrap(),
            Bson::DateTime(_)
        ));
        assert!(matches!(
            filter_value("p", Some(&Bson::Decimal128("1".parse().unwrap())), "1.25").unwrap(),
            Bson::Decimal128(_)
        ));
        assert_eq!(
            filter_value("tags", Some(&Bson::Array(vec![Bson::String("a".into())])), "a").unwrap(),
            Bson::String("a".into())
        );
    }

    #[test]
    fn every_operator_builds_a_query_document() {
        let int = Bson::Int32(1);
        let text = Bson::String("x".into());
        let q = |c: FilterCond, sample: &Bson| condition(&c, Some(sample)).unwrap();
        assert_eq!(q(cond("_id", FilterOp::Eq, &["5"]), &int), doc! { "_id": { "$eq": 5_i64 } });
        assert_eq!(
            q(cond("_id", FilterOp::Ne, &["5"]), &int),
            doc! { "_id": { "$nin": [5_i64, Bson::Null] } }
        );
        assert_eq!(q(cond("_id", FilterOp::Le, &["10"]), &int), doc! { "_id": { "$lte": 10_i64 } });
        assert_eq!(
            q(cond("name", FilterOp::Contains, &["ITEM 0.0"]), &text),
            doc! { "name": { "$regex": r"ITEM 0\.0", "$options": "i" } }
        );
        assert_eq!(
            q(cond("name", FilterOp::NotContains, &["x"]), &text),
            doc! { "name": { "$not": values::regex("x", "i"), "$ne": Bson::Null } }
        );
        assert_eq!(
            q(cond("name", FilterOp::StartsWith, &["item 01"]), &text),
            doc! { "name": { "$regex": "^item 01", "$options": "i" } }
        );
        assert_eq!(
            q(cond("name", FilterOp::EndsWith, &["999"]), &text),
            doc! { "name": { "$regex": "999$", "$options": "i" } }
        );
        assert_eq!(
            q(cond("name", FilterOp::Like, &["item%"]), &text),
            doc! { "name": { "$regex": "^item.*$", "$options": "s" } }
        );
        assert_eq!(
            q(cond("_id", FilterOp::In, &["1", "2"]), &int),
            doc! { "_id": { "$in": [1_i64, 2_i64] } }
        );
        assert_eq!(
            q(cond("_id", FilterOp::NotIn, &["1"]), &int),
            doc! { "_id": { "$nin": [1_i64, Bson::Null] } }
        );
        assert_eq!(
            q(cond("_id", FilterOp::Between, &["10", "19"]), &int),
            doc! { "_id": { "$gte": 10_i64, "$lte": 19_i64 } }
        );
        assert_eq!(
            q(cond("note", FilterOp::IsNull, &[]), &text),
            doc! { "note": { "$eq": Bson::Null } }
        );
        assert_eq!(
            q(cond("note", FilterOp::IsNotNull, &[]), &text),
            doc! { "note": { "$ne": Bson::Null } }
        );
        // Text matches on a number field use its text form.
        let on_number = q(cond("n", FilterOp::Contains, &["12"]), &int);
        assert!(on_number.contains_key("$expr"), "{on_number}");
        assert!(condition(&cond("_id", FilterOp::Between, &["1"]), Some(&int)).is_err());
        assert_eq!(
            combine(vec![doc! { "a": 1 }, doc! { "b": 2 }], true),
            doc! { "$or": [ { "a": 1 }, { "b": 2 } ] }
        );
        assert_eq!(combine(vec![doc! { "a": 1 }], false), doc! { "a": 1 });
        assert!(combine(vec![], false).is_empty());
    }

    #[test]
    fn raw_filters_parse_extended_json() {
        let filter = raw_filter(r#"{ "_id": { "$oid": "65f0c0ffee0000000000abcd" }, "n": { "$gt": 2 } }"#).unwrap();
        assert!(matches!(filter.get("_id"), Some(Bson::ObjectId(_))));
        assert!(raw_filter("  ").unwrap().is_empty());
        assert!(raw_filter("[1]").is_err());
    }

    #[test]
    fn guards_use_the_values_read_before_the_write() {
        let current = doc! { "_id": 10, "name": "item 0010", "price": 1.5 };
        let change = RowChange::Update {
            key: vec![Field::new("_id", Value::Int(10))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![Field::new("name", Value::Text("item 0010".into()))],
            version: None,
            force: false,
        };
        assert!(still_matches(&change, Some(&current)));
        assert_eq!(
            guard(&change, &current),
            doc! { "_id": { "$eq": 10 }, "name": { "$eq": "item 0010" } }
        );
        let stale = RowChange::Update {
            key: vec![Field::new("_id", Value::Int(10))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![Field::new("name", Value::Text("other".into()))],
            version: None,
            force: false,
        };
        assert!(!still_matches(&stale, Some(&current)));
        assert!(!still_matches(&change, None));
        let RowChange::Update {
            key,
            values,
            original,
            version,
            ..
        } = stale
        else {
            unreachable!()
        };
        let forced = RowChange::Update {
            key,
            values,
            original,
            version,
            force: true,
        };
        assert!(still_matches(&forced, Some(&current)));
        assert_eq!(guard(&forced, &current), doc! { "_id": { "$eq": 10 } });
        let set = set_document(&[Field::new("price", Value::Int(2))], &[], &current).unwrap();
        assert_eq!(set, doc! { "price": 2.0 });
        let inserted = insert_document(&[Field::new("a.b", Value::Int(1))]).unwrap();
        assert!(matches!(inserted.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(inserted.get_document("a").unwrap(), &doc! { "b": 1 });
    }

    fn update(values: Vec<Field>, original: Vec<Field>) -> RowChange {
        RowChange::Update {
            key: vec![Field::new("_id", values::exact(r#"{"$oid":"65f0c0ffee0000000000abcd"}"#))],
            values,
            original,
            version: None,
            force: false,
        }
    }

    #[test]
    fn nested_edits_build_dotted_set_and_unset() {
        let current = doc! {
            "_id": ObjectId::parse_str("65f0c0ffee0000000000abcd").unwrap(),
            "address": { "city": "Oslo", "zip": "0150", "geo": { "lat": 59.9, "n": 5_i64 } },
            "tags": ["a", "b"],
            "stock": 3,
        };
        let values = vec![
            Field::new("address.city", values::exact(r#""Bergen""#)),
            Field::new("address.zip", values::unset()),
            Field::new("address.geo", values::exact(r#"{ "lat": 60, "n": 6 }"#)),
            Field::new("tags.1", values::exact(r#"{"$numberLong":"7"}"#)),
            Field::new("stock", Value::Int(4)),
            Field::new("added.deep", values::exact(r#"{"$date":"2026-01-02T00:00:00Z"}"#)),
        ];
        let update = update_document(&values, &[], &current).unwrap();
        let set = update.get_document("$set").unwrap();
        assert_eq!(set.get("address.city"), Some(&Bson::String("Bergen".into())));
        // Numbers of an edited document keep their stored types: a double stays a double, a long a long.
        assert_eq!(set.get("address.geo"), Some(&Bson::Document(doc! { "lat": 60.0, "n": 6_i64 })));
        assert_eq!(set.get("tags.1"), Some(&Bson::Int64(7)));
        assert_eq!(set.get("stock"), Some(&Bson::Int32(4)));
        assert!(matches!(set.get("added.deep"), Some(Bson::DateTime(_))));
        assert!(!set.contains_key("address"), "the top-level field is not rewritten: {set}");
        assert_eq!(update.get_document("$unset").unwrap(), &doc! { "address.zip": "" });

        let only_unset = update_document(&[Field::new("stock", values::unset())], &[], &current).unwrap();
        assert_eq!(only_unset, doc! { "$unset": { "stock": "" } });
        for bad in [
            vec![
                Field::new("address", values::unset()),
                Field::new("address.city", values::exact("1")),
            ],
            vec![Field::new("_id", values::exact("1"))],
            vec![Field::new("a.$.b", values::exact("1"))],
            vec![Field::new("a..b", values::exact("1"))],
            vec![Field::new("a", values::exact("{ nope"))],
        ] {
            assert!(update_document(&bad, &[], &current).is_err(), "{bad:?}");
        }
        assert!(overlaps("a", "a.b") && overlaps("a.b", "a") && overlaps("a", "a"));
        assert!(!overlaps("a", "ab") && !overlaps("a.b", "a.c"));
    }

    #[test]
    fn exact_originals_compare_as_loaded_and_conflict_on_changed_paths() {
        let current = doc! { "_id": 1, "address": { "city": "Oslo", "zip": "0150" }, "price": 10.0 };
        let loaded = |path: &str, json: &str| Field::new(path, values::exact(json));
        let edit = |original: Vec<Field>| update(vec![Field::new("address.city", values::exact(r#""Bergen""#))], original);
        // The value read before still holds: a whole double sent back as 10 matches 10.0, a missing field matches null.
        assert!(still_matches(
            &edit(vec![loaded("address.city", r#""Oslo""#), loaded("price", "10")]),
            Some(&current)
        ));
        assert!(still_matches(&edit(vec![loaded("added", "null")]), Some(&current)));
        // Someone changed the edited path since: a conflict.
        let stale = edit(vec![loaded("address.city", r#""Lisbon""#)]);
        assert!(!still_matches(&stale, Some(&current)));
        // Another field of the same document changed: no conflict on this path.
        assert!(still_matches(
            &edit(vec![loaded("address.city", r#""Oslo""#)]),
            Some(&doc! { "_id": 1, "address": { "city": "Oslo", "zip": "9999" } })
        ));
        // The guard of the write repeats the paths with the values just read.
        assert_eq!(
            guard(&edit(vec![loaded("address.city", r#""Oslo""#)]), &current),
            doc! { "_id": { "$eq": 1 }, "address.city": { "$eq": "Oslo" } }
        );
        // The conflict shows the path as it is now, in the form it was loaded.
        let fields = current_fields(&stale, &current);
        let city = fields.iter().find(|field| field.column == "address.city").unwrap();
        assert_eq!(city.value, values::exact(r#""Oslo""#));
        assert!(fields.iter().any(|field| field.column == "address"));
        // Exact keys find their document.
        let key = values::exact(r#"{"$oid":"65f0c0ffee0000000000abcd"}"#);
        assert_eq!(
            key_query(&[Field::new("_id", key)]).unwrap(),
            doc! { "_id": ObjectId::parse_str("65f0c0ffee0000000000abcd").unwrap() }
        );
        let inserted = insert_document(&[
            loaded("n", r#"{"$numberLong":"5"}"#),
            loaded("at", r#"{"$date":"2026-01-02T00:00:00Z"}"#),
        ])
        .unwrap();
        assert_eq!(inserted.get("n"), Some(&Bson::Int64(5)));
        assert!(insert_document(&[loaded("n", "{ nope")]).is_err());
    }
}
