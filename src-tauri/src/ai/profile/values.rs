//! Value hints (P4): which values a column takes, or which format they follow, from a bounded sample read while
//! learning. Only where the user lets the assistant read data (`gateway::assistant::reads_allowed`) and the caller asked
//! for it; every read goes through the gateway as the assistant (Read only, audited, time-bounded):
//!
//! - SQL engines: [`Engine::distinct_values_statement`] (`SELECT DISTINCT c FROM (SELECT c FROM t LIMIT 1000) s LIMIT
//!   21`, identifiers quoted by the engine's dialect) through `gateway::assistant::read`;
//! - MongoDB: `mongo.distinct` (at most 20 values, a short `maxTimeMS`) through `gateway::assistant::invoke_read`;
//! - other engines (Elasticsearch, Redis): no bounded distinct read, so no hints, and the outcome says so.
//!
//! Sensitive columns ([`redact::is_sensitive_name`]), primary keys and types unlikely to hold few values are never read;
//! excluded objects are not in the profile to begin with. Values that look like secrets are dropped, and values of
//! formats that carry personal data or identifiers (emails, phone numbers, URLs, UUIDs, JSON) are never kept, only the
//! format. The work is bounded per object, per build and in time, and stops with the build's cancellation.
//!
//! [`Engine::distinct_values_statement`]: crate::contract::Engine::distinct_values_statement

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{json, Value as Json};

use super::{
    build::{Progress, ProgressFn, Step},
    model::{ColumnProfile, DatabaseProfile, ObjectProfile, ValueHint},
    redact,
};
use crate::{
    contract::{cancelled, CancellationToken, Capability, ColumnInfo, NodeKind, ObjectRef, Value, ValueKind},
    error::AppResult,
    gateway::{self, assistant::ReadAudit, Context},
    runtime::{ConnectionManager, Handle},
};

/// Columns read per object, and per build.
pub const MAX_COLUMNS_PER_OBJECT: usize = 30;
pub const MAX_COLUMNS: usize = 400;
/// Rows a SQL sample looks at, per column.
pub const SAMPLE_ROWS: u64 = 1_000;
/// A column with at most this many distinct values (of at most [`MAX_VALUE_CHARS`] characters) gets them listed.
pub const MAX_ENUM_VALUES: usize = 20;
pub const MAX_VALUE_CHARS: usize = 64;
/// How long all the reads of one build may take together.
pub const BUDGET: Duration = Duration::from_secs(60);
/// The server-side time bound of one MongoDB read.
const MONGO_MAX_TIME_MS: u64 = 3_000;
/// The engine whose connections answer `mongo.distinct`.
const MONGODB: &str = "mongodb";
/// Failed reads in one object before its other columns are left alone.
const MAX_FAILURES_PER_OBJECT: usize = 2;

/// The assistant's gateway context on the connection being learned: reads go through the gateway as the assistant.
pub struct Sampler<'a> {
    pub manager: &'a ConnectionManager,
    pub context: Context,
}

/// What value sampling did during a build.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuesOutcome {
    /// Columns read.
    pub columns: usize,
    /// Columns that got a hint.
    pub hinted: usize,
    /// Reads that failed (the column got no hint).
    pub failed: usize,
    /// Why sampling stopped early or did not run (an engine without a bounded read, a refusal, the time budget).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// How an engine reads distinct values.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Path {
    Sql,
    Mongo,
}

fn path_of(handle: &Handle) -> Option<Path> {
    let probe = ObjectRef::table("db", None, "t");
    let column = ColumnInfo::result("c", "text", ValueKind::Text);
    if handle.conn.capabilities().contains(Capability::Query) && handle.engine.distinct_values_statement(&probe, &column, 1, 1).is_some() {
        Some(Path::Sql)
    } else if handle.engine.id() == MONGODB {
        Some(Path::Mongo)
    } else {
        None
    }
}

/// Small integer types, by the engines' spellings (`smallint`, `tinyint`, `int2`, `bit`, ClickHouse `UInt8`).
fn small_int(type_name: &str) -> bool {
    let lower = type_name.to_ascii_lowercase();
    (["tiny", "small", "int1", "int2", "bit", "bool"]
        .iter()
        .any(|part| lower.contains(part))
        && !lower.contains("128"))
        || ["Int8", "UInt8", "Int16", "UInt16"].iter().any(|t| type_name.contains(t))
}

/// Types that never hold a short list of values, or that cannot be compared for DISTINCT on some engines.
const SKIPPED_TYPES: [&str; 16] = [
    "blob",
    "clob",
    "ntext",
    "image",
    "xml",
    "binary",
    "bytea",
    "geometry",
    "geography",
    "point",
    "polygon",
    "vector",
    "tsvector",
    "range",
    "json",
    "lob",
];

/// Whether a column is worth reading: not sensitive, not a key, and of a type that can hold a short list of values.
pub(super) fn sampled(column: &ColumnProfile) -> bool {
    // `prefs.{key}` stands for many fields whose names were values.
    if column.sensitive || column.primary || redact::is_sensitive_name(&column.name) || column.name.contains(redact::KEY_PLACEHOLDER) {
        return false;
    }
    let lower = column.type_name.to_ascii_lowercase();
    if SKIPPED_TYPES.iter().any(|skipped| lower.contains(skipped)) {
        return false;
    }
    match column.kind {
        ValueKind::Bool | ValueKind::Text | ValueKind::Other => true,
        ValueKind::Int => small_int(&column.type_name),
        _ => false,
    }
}

/// Tables and collections; views may be expensive to read.
fn sampled_object(object: &ObjectProfile) -> bool {
    object.detailed && (object.kind == NodeKind::TABLE || object.kind == NodeKind::COLLECTION)
}

/// One value as text: strings, numbers and booleans; nulls, binary, documents and the like are left out.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Bool(value) => Some(value.to_string()),
        Value::Int(value) => Some(value.to_string()),
        Value::Decimal(text) | Value::Text(text) | Value::Interval(text) => Some(text.clone()),
        Value::Other { text, .. } => Some(text.clone()),
        Value::Uuid(text) => Some(text.clone()),
        _ => None,
    }
}

fn json_text_of(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Bool(value) => Some(value.to_string()),
        Json::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The formats a sample can show.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !text.contains(char::is_whitespace)
}

fn is_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["http://", "https://", "ftp://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
        && !text.contains(char::is_whitespace)
}

fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..10].iter().enumerate().all(|(index, b)| match index {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
        && (bytes.len() == 10 || matches!(bytes[10], b'T' | b' '))
}

fn is_json(text: &str) -> bool {
    let trimmed = text.trim();
    ((trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && serde_json::from_str::<Json>(trimmed).is_ok()
}

fn is_numeric(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
        && digits.chars().filter(|c| *c == '.').count() <= 1
        && digits.chars().any(|c| c.is_ascii_digit())
}

/// Digits with the separators phone numbers use, and at least a `+` or one separator (plain digits are numeric).
fn is_phone(text: &str) -> bool {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    (7..=20).contains(&digits)
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')' | '.'))
        && (text.starts_with('+') || text.chars().any(|c| matches!(c, '-' | ' ' | '(' | ')')))
        && !is_iso_date(text)
}

/// Formats whose values are never kept (personal data, identifiers, documents), most specific first.
type FormatTest = fn(&str) -> bool;

const PRIVATE_FORMATS: [(&str, FormatTest); 5] = [
    ("uuid", is_uuid),
    ("email", is_email),
    ("url", is_url),
    ("json", is_json),
    ("phone-ish", is_phone),
];

/// The hint a sample gives: a private format, an ISO date, the values (when few and short), or a numeric format.
/// `more` says the sample had more distinct values than it returned.
pub fn hint_of(column: &ColumnProfile, sample: &[String], more: bool) -> Option<ValueHint> {
    let values: Vec<&str> = sample.iter().map(String::as_str).filter(|value| !value.is_empty()).collect();
    if values.is_empty() {
        return None;
    }
    let all = |test: FormatTest| values.iter().all(|value| test(value));
    let format = |name: &str| {
        Some(ValueHint {
            column: column.name.clone(),
            values: Vec::new(),
            format: Some(name.to_owned()),
        })
    };
    let textual = matches!(column.kind, ValueKind::Text | ValueKind::Other);
    if textual {
        if let Some((name, _)) = PRIVATE_FORMATS.iter().find(|(_, test)| all(*test)) {
            return format(name);
        }
        if all(is_iso_date) {
            return format("iso-date");
        }
    }
    // A value that looks like a secret anywhere means the column is not listed at all.
    if values.iter().any(|value| redact::scrub(value) != *value) {
        return None;
    }
    let few = !more && values.len() <= MAX_ENUM_VALUES && values.iter().all(|value| value.chars().count() <= MAX_VALUE_CHARS);
    if few {
        let mut listed: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        listed.sort();
        listed.dedup();
        return Some(ValueHint {
            column: column.name.clone(),
            values: listed,
            format: None,
        });
    }
    if textual && all(is_numeric) {
        return format("numeric-string");
    }
    None
}

/// `profile` without value hints: the view the assistant gets where it may not read data now.
pub fn strip_value_hints(profile: &mut DatabaseProfile) {
    for object in &mut profile.objects {
        object.value_hints.clear();
    }
}

/// The columns a build reads, per object index, within the caps.
fn plan(profile: &DatabaseProfile, done: &HashSet<usize>) -> Vec<(usize, Vec<usize>)> {
    let mut planned = Vec::new();
    let mut total = 0;
    for (index, object) in profile.objects.iter().enumerate() {
        if total >= MAX_COLUMNS {
            break;
        }
        if !sampled_object(object) || done.contains(&index) {
            continue;
        }
        let room = MAX_COLUMNS_PER_OBJECT.min(MAX_COLUMNS - total);
        let columns: Vec<usize> = object
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| sampled(column))
            .map(|(column, _)| column)
            .take(room)
            .collect();
        if !columns.is_empty() {
            total += columns.len();
            planned.push((index, columns));
        }
    }
    planned
}

/// What one read gave: the values as text, and whether there were more distinct values than returned.
type Sample = (Vec<String>, bool);

async fn read_sql(
    sampler: &Sampler<'_>,
    handle: &Handle,
    database: &str,
    object: &ObjectProfile,
    column: &ColumnProfile,
) -> AppResult<Sample> {
    let database = (!database.is_empty()).then_some(database);
    let reference = ObjectRef::new(database, object.schema.as_deref(), &object.name, object.kind.clone());
    let info = ColumnInfo::result(&column.name, &column.type_name, column.kind.clone());
    let limit = MAX_ENUM_VALUES as u64 + 1;
    let statement = handle
        .engine
        .distinct_values_statement(&reference, &info, SAMPLE_ROWS, limit)
        .ok_or_else(|| crate::contract::unsupported_capability("value sampling"))?;
    let result = gateway::assistant::read_audited(sampler.manager, &sampler.context, &statement, database, ReadAudit::Failures).await?;
    let more = result.rows.len() > MAX_ENUM_VALUES || result.truncated;
    let values = result.rows.iter().filter_map(|row| row.first().and_then(text_of)).collect();
    Ok((values, more))
}

async fn read_mongo(sampler: &Sampler<'_>, database: &str, object: &ObjectProfile, column: &ColumnProfile) -> AppResult<Sample> {
    let args = json!({
        "database": database,
        "collection": object.name,
        "field": column.name,
        "limit": MAX_ENUM_VALUES,
        "maxTimeMS": MONGO_MAX_TIME_MS,
    });
    let result =
        gateway::assistant::invoke_read_audited(sampler.manager, &sampler.context, "mongo.distinct", args, ReadAudit::Failures).await?;
    let more = result.get("truncated").and_then(Json::as_bool).unwrap_or(false);
    let values = result
        .get("values")
        .and_then(Json::as_array)
        .map(|values| values.iter().filter_map(json_text_of).collect())
        .unwrap_or_default();
    Ok((values, more))
}

/// The one audit entry of a build's value reads (each failed read has its own, each refusal too).
fn audit_values(sampler: &Sampler<'_>, handle: &Handle, database: &str, path: Path, outcome: &ValuesOutcome) {
    if outcome.columns == 0 {
        return;
    }
    let read = match path {
        Path::Sql => "SELECT DISTINCT",
        Path::Mongo => "mongo.distinct",
    };
    let stopped = outcome.skipped.as_deref().map(|why| format!("; {why}")).unwrap_or_default();
    gateway::assistant::audit_batch(
        sampler.manager,
        &sampler.context,
        handle.engine.id(),
        format!(
            "Learning {}: {} value reads ({read}, at most {SAMPLE_ROWS} rows each)",
            shown(database),
            outcome.columns
        ),
        format!(
            "{} reads, {} failed, {} value hints{stopped}",
            outcome.columns, outcome.failed, outcome.hinted
        ),
    );
}

/// A database name as the audit log shows it.
pub(super) fn shown(database: &str) -> String {
    if database.is_empty() {
        "this connection".to_owned()
    } else {
        database.to_owned()
    }
}

/// Reads value hints into `profile` (see the module docs). Stops with `cancel` (the error is `CANCELLED`); a refusal by
/// the gateway or the time budget ends sampling early and keeps what was read. Sets `data_read` when any hint was kept.
pub async fn sample(
    sampler: &Sampler<'_>,
    handle: &Handle,
    profile: &mut DatabaseProfile,
    cancel: &CancellationToken,
    progress: ProgressFn<'_>,
) -> AppResult<ValuesOutcome> {
    sample_except(sampler, handle, profile, &HashSet::new(), cancel, progress).await
}

/// [`sample`], leaving alone the objects (by index) whose values the engine's extras already sampled: their hints
/// stay as the extras left them.
pub async fn sample_except(
    sampler: &Sampler<'_>,
    handle: &Handle,
    profile: &mut DatabaseProfile,
    done: &HashSet<usize>,
    cancel: &CancellationToken,
    progress: ProgressFn<'_>,
) -> AppResult<ValuesOutcome> {
    let mut outcome = ValuesOutcome::default();
    let Some(path) = path_of(handle) else {
        outcome.skipped = Some(format!(
            "{} has no bounded way to read distinct values, so no value hints were taken",
            handle.engine.descriptor().label
        ));
        return Ok(outcome);
    };
    let planned = plan(profile, done);
    let total: usize = planned.iter().map(|(_, columns)| columns.len()).sum();
    let started = Instant::now();
    let database = profile.database.clone();
    let mut done = 0;
    'objects: for (index, columns) in planned {
        let mut failures = 0;
        let mut hints = Vec::new();
        for column_index in columns {
            if cancel.is_cancelled() {
                audit_values(sampler, handle, &database, path, &outcome);
                return Err(cancelled());
            }
            let Some(remaining) = BUDGET.checked_sub(started.elapsed()).filter(|left| !left.is_zero()) else {
                outcome.skipped = Some(format!("Value sampling stopped after {} seconds", BUDGET.as_secs()));
                profile.objects[index].value_hints = hints;
                break 'objects;
            };
            let object = &profile.objects[index];
            let column = &object.columns[column_index];
            progress(Progress {
                object: Some(object.qualified()),
                ..Progress::new(Step::Values, done, total)
            });
            done += 1;
            let read = async {
                match path {
                    Path::Sql => read_sql(sampler, handle, &database, object, column).await,
                    Path::Mongo => read_mongo(sampler, &database, object, column).await,
                }
            };
            // Counted when started, so a cancelled read is in the audit entry too.
            outcome.columns += 1;
            let outcome_of_read = tokio::select! {
                read = tokio::time::timeout(remaining, read) => read,
                _ = cancel.cancelled() => {
                    audit_values(sampler, handle, &database, path, &outcome);
                    return Err(cancelled());
                }
            };
            match outcome_of_read {
                Ok(Ok((values, more))) => {
                    if let Some(hint) = hint_of(column, &values, more) {
                        hints.push(hint);
                    }
                }
                Ok(Err(error)) if gateway::assistant::is_refusal(&error) => {
                    outcome.skipped = Some(format!("The workspace's rules refused the assistant's reads: {error}"));
                    profile.objects[index].value_hints = hints;
                    break 'objects;
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "a value sample failed");
                    outcome.failed += 1;
                    failures += 1;
                    if failures >= MAX_FAILURES_PER_OBJECT {
                        break;
                    }
                }
                Err(_) => {
                    outcome.skipped = Some(format!("Value sampling stopped after {} seconds", BUDGET.as_secs()));
                    profile.objects[index].value_hints = hints;
                    break 'objects;
                }
            }
        }
        profile.objects[index].value_hints = hints;
    }
    progress(Progress::new(Step::Values, total, total));
    outcome.hinted = profile.objects.iter().map(|object| object.value_hints.len()).sum();
    profile.data_read = outcome.hinted > 0;
    audit_values(sampler, handle, &database, path, &outcome);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, type_name: &str, kind: ValueKind) -> ColumnProfile {
        ColumnProfile {
            name: name.into(),
            type_name: type_name.into(),
            kind,
            nullable: true,
            primary: false,
            comment: None,
            default: None,
            sensitive: false,
            presence: None,
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn samples_become_values_or_formats() {
        let status = column("status", "text", ValueKind::Text);
        let hint = hint_of(&status, &strings(&["paid", "new", "shipped", "new"]), false).unwrap();
        assert_eq!((hint.values, hint.format), (strings(&["new", "paid", "shipped"]), None));
        // Too many, or too long, values give no list.
        assert!(hint_of(&status, &strings(&["a"; 3]), true).is_none());
        assert!(hint_of(&status, &[("x".repeat(65))], false).is_none());
        let format = |values: &[&str], more: bool| hint_of(&status, &strings(values), more).and_then(|hint| hint.format);
        assert_eq!(format(&["a@example.com", "b@example.org"], false).as_deref(), Some("email"));
        assert_eq!(format(&["a2b3c4d5-e6f7-4890-abcd-ef0123456789"], false).as_deref(), Some("uuid"));
        assert_eq!(format(&["https://example.com/a"], true).as_deref(), Some("url"));
        assert_eq!(format(&["2026-01-02", "2026-01-03T04:05:06Z"], true).as_deref(), Some("iso-date"));
        assert_eq!(format(&["{\"a\": 1}", "[1, 2]"], false).as_deref(), Some("json"));
        assert_eq!(format(&["+1 555 123 4567", "(555) 123-4567"], false).as_deref(), Some("phone-ish"));
        assert_eq!(format(&["12", "13.5", "-4"], true).as_deref(), Some("numeric-string"));
        // Few numeric strings are listed: codes are more useful than a format.
        assert_eq!(hint_of(&status, &strings(&["1", "2"]), false).unwrap().values, strings(&["1", "2"]));
        // Emails are never listed, however few.
        assert!(hint_of(&status, &strings(&["a@example.com"]), false).unwrap().values.is_empty());
        // A secret-looking value drops the column.
        assert!(hint_of(&status, &strings(&["new", "sk-abcdefghijklmnopqrstuvwx1234"]), false).is_none());
        assert!(hint_of(&status, &[], false).is_none());
    }

    #[test]
    fn only_low_cardinality_looking_columns_are_read() {
        assert!(sampled(&column("status", "varchar(20)", ValueKind::Text)));
        assert!(sampled(&column("active", "boolean", ValueKind::Bool)));
        assert!(sampled(&column("level", "smallint", ValueKind::Int)));
        assert!(sampled(&column("level", "Nullable(UInt8)", ValueKind::Int)));
        assert!(sampled(&column("mood", "mood_enum", ValueKind::Other)));
        assert!(!sampled(&column("count", "bigint", ValueKind::Int)));
        assert!(!sampled(&column("created", "timestamp", ValueKind::Timestamp)));
        assert!(!sampled(&column("shape", "geometry", ValueKind::Other)));
        assert!(!sampled(&column("body", "nclob", ValueKind::Text)));
        assert!(!sampled(&column("password", "text", ValueKind::Text)));
        let mut key = column("code", "text", ValueKind::Text);
        key.primary = true;
        assert!(!sampled(&key));
        let mut flagged = column("status", "text", ValueKind::Text);
        flagged.sensitive = true;
        assert!(!sampled(&flagged));
    }
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod engine_tests;
