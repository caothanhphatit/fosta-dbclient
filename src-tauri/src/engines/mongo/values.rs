//! BSON and the contract's typed values.
//!
//! Reading: ObjectId → `Value::ObjectId`, embedded document → `Value::Document` (key order kept), array →
//! `Value::Array`, int32 and int64 → `Value::Int`, double → `Float`, Decimal128 → `Decimal`, date → `TimestampTz`
//! (UTC), binary → `Bytes` (`Uuid` for subtype 4), string → `Text`; regex, timestamp, JavaScript, MinKey and MaxKey are
//! `Other` with their relaxed Extended JSON text.
//!
//! Writing keeps the stored BSON type: a new value is coerced against the field's current value (an int32 stays an
//! int32, a Decimal128 a Decimal128, a date a date, JSON text for a document or array becomes a document or array with
//! the numbers typed as before). Explicit Extended JSON (`{"$numberLong": "5"}`) changes the type on purpose.

use std::collections::HashMap;

use mongodb::bson::{oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Decimal128, Document, Regex};

use crate::{
    contract::{
        value::{normalize_uuid, parse_timestamptz, unhex, uuid_from_bytes},
        ColumnInfo, Field, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

pub const ID_FIELD: &str = "_id";

/// The BSON type name as the server's `$type` spells it.
pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Null => "null",
        Bson::Undefined => "undefined",
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
        Bson::Binary(_) => "binData",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Symbol(_) => "symbol",
        Bson::DbPointer(_) => "dbPointer",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// The neutral kind a field of this BSON type is edited as. Arrays are edited as JSON text: their items may mix types.
pub fn kind_of(value: &Bson) -> ValueKind {
    match value {
        Bson::Int32(_) | Bson::Int64(_) => ValueKind::Int,
        Bson::Double(_) => ValueKind::Float,
        Bson::Decimal128(_) => ValueKind::Decimal,
        Bson::String(_) | Bson::Symbol(_) => ValueKind::Text,
        Bson::Boolean(_) => ValueKind::Bool,
        Bson::DateTime(_) => ValueKind::TimestampTz,
        Bson::ObjectId(_) => ValueKind::ObjectId,
        Bson::Document(_) => ValueKind::Document,
        Bson::Array(_) => ValueKind::Json,
        Bson::Binary(binary) if is_uuid(binary) => ValueKind::Uuid,
        Bson::Binary(_) => ValueKind::Bytes,
        _ => ValueKind::Other,
    }
}

fn is_uuid(binary: &Binary) -> bool {
    binary.subtype == BinarySubtype::Uuid && binary.bytes.len() == 16
}

/// Relaxed Extended JSON text of a value (`{"$regularExpression": …}`), for types without a neutral value.
pub fn extended_json(value: &Bson) -> String {
    value.clone().into_relaxed_extjson().to_string()
}

pub fn to_value(value: &Bson) -> Value {
    match value {
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Boolean(flag) => Value::Bool(*flag),
        Bson::Int32(number) => Value::Int(i64::from(*number)),
        Bson::Int64(number) => Value::Int(*number),
        Bson::Double(number) => Value::Float(*number),
        Bson::Decimal128(number) => Value::Decimal(number.to_string()),
        Bson::String(text) | Bson::Symbol(text) => Value::Text(text.clone()),
        Bson::ObjectId(id) => Value::ObjectId(id.to_hex()),
        Bson::DateTime(date) => match chrono::DateTime::from_timestamp_millis(date.timestamp_millis()) {
            Some(date) => Value::TimestampTz(date.fixed_offset()),
            None => other(value),
        },
        Bson::Binary(binary) if is_uuid(binary) => Value::Uuid(uuid_from_bytes(&binary.bytes)),
        Bson::Binary(binary) => Value::Bytes(binary.bytes.clone()),
        Bson::Document(document) => Value::Document(document.iter().map(|(key, value)| (key.clone(), to_value(value))).collect()),
        Bson::Array(items) => Value::Array(items.iter().map(to_value).collect()),
        _ => other(value),
    }
}

fn other(value: &Bson) -> Value {
    Value::Other {
        type_name: type_name(value).to_owned(),
        text: extended_json(value),
    }
}

/// Relaxed or canonical Extended JSON (what nested values are shown in) back to BSON with its types.
pub fn parse_extended_json(text: &str) -> Option<Bson> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    Bson::try_from(value).ok()
}

/// A JSON object in relaxed Extended JSON (`$oid`, `$date`, `$numberDecimal` …) as a document, keys in order.
pub fn parse_document(text: &str) -> Result<Document, String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("expected a JSON object".to_owned());
    }
    match Bson::try_from(value).map_err(|error| error.to_string())? {
        Bson::Document(document) => Ok(document),
        _ => Err("expected a JSON object".to_owned()),
    }
}

fn date_of(date: chrono::DateTime<chrono::FixedOffset>) -> Bson {
    Bson::DateTime(DateTime::from_millis(date.timestamp_millis()))
}

fn decimal(text: &str) -> Option<Bson> {
    text.trim().parse::<Decimal128>().ok().map(Bson::Decimal128)
}

fn uuid_bytes(text: &str) -> Option<Vec<u8>> {
    let normalized = normalize_uuid(text)?;
    unhex(&normalized.replace('-', ""), true)
}

/// A value as BSON without a stored type to follow (inserts, new fields, filters): ints that fit stay int32 (as the
/// shell writes them), JSON text parses as Extended JSON.
pub fn from_value(value: &Value) -> AppResult<Bson> {
    Ok(match value {
        Value::Null => Bson::Null,
        Value::Bool(flag) => Bson::Boolean(*flag),
        Value::Int(number) => i32::try_from(*number).map_or(Bson::Int64(*number), Bson::Int32),
        Value::Float(number) => Bson::Double(*number),
        Value::Decimal(text) => decimal(text).ok_or_else(|| AppError::new(format!("'{text}' is not a valid decimal number")))?,
        Value::Text(text) | Value::Interval(text) => Bson::String(text.clone()),
        Value::Bytes(bytes) => Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: bytes.clone(),
        }),
        Value::Date(date) => date_of(date.and_time(chrono::NaiveTime::MIN).and_utc().fixed_offset()),
        Value::Time(time) => Bson::String(time.format("%H:%M:%S%.f").to_string()),
        Value::Timestamp(stamp) => date_of(stamp.and_utc().fixed_offset()),
        Value::TimestampTz(stamp) => date_of(*stamp),
        Value::Uuid(text) => Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uuid_bytes(text).ok_or_else(|| AppError::new(format!("'{text}' is not a valid UUID")))?,
        }),
        Value::Json(text) => {
            parse_extended_json(text).ok_or_else(|| AppError::new(format!("'{text}' is not valid JSON or Extended JSON")))?
        }
        Value::Array(items) => Bson::Array(items.iter().map(from_value).collect::<AppResult<_>>()?),
        Value::ObjectId(text) => Bson::ObjectId(object_id(text)?),
        Value::Document(fields) => Bson::Document(
            fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), from_value(value)?)))
                .collect::<AppResult<Document>>()?,
        ),
        Value::Other { type_name, text } if type_name == "int32" || type_name == "int" => text
            .trim()
            .parse::<i32>()
            .map(Bson::Int32)
            .map_err(|_| AppError::new(format!("'{text}' is not a valid 32-bit integer")))?,
        Value::Other { text, .. } => parse_extended_json(text).unwrap_or_else(|| Bson::String(text.clone())),
    })
}

fn object_id(text: &str) -> AppResult<ObjectId> {
    let hex = text
        .trim()
        .trim_start_matches("ObjectId(")
        .trim_end_matches(')')
        .trim_matches(|c| c == '"' || c == '\'');
    ObjectId::parse_str(hex).map_err(|_| AppError::new(format!("'{text}' is not a valid ObjectId (24 hex characters)")))
}

/// A key value as BSON: an `_id` typed as text that looks like an ObjectId is one (the grid shows ObjectIds as hex).
pub fn key_to_bson(column: &str, value: &Value) -> AppResult<Bson> {
    match value {
        Value::Text(text) if column == ID_FIELD && ObjectId::parse_str(text.trim()).is_ok() => Ok(Bson::ObjectId(object_id(text)?)),
        other => from_value(other),
    }
}

/// JSON text of a nested value loses BSON types: relaxed Extended JSON writes every number as plain JSON (a small long
/// reads back as an int, a whole double as an int), and the grid shows dates, decimals and ObjectIds as text. Where the
/// stored value at the same place had another type, the edited value takes that type again when it fits.
pub fn match_types(edited: Bson, original: &Bson) -> Bson {
    match (edited, original) {
        (Bson::Document(edited), Bson::Document(original)) => Bson::Document(
            edited
                .into_iter()
                .map(|(key, value)| {
                    let value = match original.get(&key) {
                        Some(before) => match_types(value, before),
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
                    Some(before) => match_types(value, before),
                    None => value,
                })
                .collect(),
        ),
        (Bson::String(text), template) if !matches!(template, Bson::String(_) | Bson::Symbol(_)) => {
            typed_from_text(template, &text).unwrap_or(Bson::String(text))
        }
        (Bson::Double(number), Bson::Decimal128(_)) => decimal(&number.to_string()).unwrap_or(Bson::Double(number)),
        (Bson::Int32(number), Bson::Int64(_)) => Bson::Int64(i64::from(number)),
        (Bson::Int32(number), Bson::Double(_)) => Bson::Double(f64::from(number)),
        // Whole doubles beyond i32 read back as longs; the conversion is exact below 2^53.
        #[allow(clippy::cast_precision_loss)]
        (Bson::Int64(number), Bson::Double(_)) => Bson::Double(number as f64),
        (Bson::Int32(number), Bson::Decimal128(_)) => decimal(&number.to_string()).unwrap_or(Bson::Int32(number)),
        (Bson::Int64(number), Bson::Decimal128(_)) => decimal(&number.to_string()).unwrap_or(Bson::Int64(number)),
        (edited, _) => edited,
    }
}

/// The text a value was typed as, when it is text.
fn text_of(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) | Value::Json(text) | Value::Other { text, .. } => Some(text),
        _ => None,
    }
}

/// A new value for a field converted to the BSON type the field has now (`template`), so editing a date, an int32, a
/// Decimal128 or an ObjectId keeps it one. Values that do not fit are refused with an example, unless they are explicit
/// Extended JSON.
pub fn coerce(column: &str, value: &Value, template: Option<&Bson>) -> AppResult<Bson> {
    if value.is_null() {
        return Ok(Bson::Null);
    }
    let Some(template) = template.filter(|template| !matches!(template, Bson::Null | Bson::Undefined)) else {
        return from_value(value);
    };
    let text = text_of(value).map(str::trim);
    let typed: Option<Bson> = match (template, value) {
        (Bson::String(_), Value::Text(text)) => Some(Bson::String(text.clone())),
        (Bson::String(_), Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Bool(_) | Value::Uuid(_)) => {
            value.display().map(Bson::String)
        }
        (Bson::Int32(_), Value::Int(number)) => Some(i32::try_from(*number).map_or(Bson::Int64(*number), Bson::Int32)),
        (Bson::Int64(_), Value::Int(number)) => Some(Bson::Int64(*number)),
        #[allow(clippy::cast_precision_loss)]
        (Bson::Double(_), Value::Int(number)) => Some(Bson::Double(*number as f64)),
        (Bson::Double(_), Value::Float(number)) => Some(Bson::Double(*number)),
        (Bson::Decimal128(_), Value::Int(number)) => decimal(&number.to_string()),
        (Bson::Decimal128(_), Value::Float(number)) => decimal(&number.to_string()),
        (Bson::Decimal128(_), Value::Decimal(text)) => decimal(text),
        (Bson::Int32(_), Value::Decimal(text)) => text.parse::<i32>().ok().map(Bson::Int32),
        (Bson::Int64(_), Value::Decimal(text)) => text.parse::<i64>().ok().map(Bson::Int64),
        (Bson::Double(_), Value::Decimal(text)) => text.parse::<f64>().ok().map(Bson::Double),
        (Bson::DateTime(_), Value::TimestampTz(stamp)) => Some(date_of(*stamp)),
        (Bson::DateTime(_), Value::Timestamp(stamp)) => Some(date_of(stamp.and_utc().fixed_offset())),
        (Bson::DateTime(_), Value::Date(date)) => Some(date_of(date.and_time(chrono::NaiveTime::MIN).and_utc().fixed_offset())),
        (Bson::Boolean(_), Value::Bool(flag)) => Some(Bson::Boolean(*flag)),
        (Bson::ObjectId(_), Value::ObjectId(hex)) => object_id(hex).ok().map(Bson::ObjectId),
        (Bson::Binary(binary), Value::Bytes(bytes)) => Some(Bson::Binary(Binary {
            subtype: binary.subtype,
            bytes: bytes.clone(),
        })),
        (Bson::Binary(_), Value::Uuid(text)) => uuid_bytes(text).map(|bytes| {
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes,
            })
        }),
        (Bson::Document(stored), Value::Document(fields)) => Some(Bson::Document(
            fields
                .iter()
                .map(|(key, value)| Ok((key.clone(), coerce(key, value, stored.get(key))?)))
                .collect::<AppResult<Document>>()?,
        )),
        (Bson::Array(stored), Value::Array(items)) => Some(Bson::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, value)| coerce(column, value, stored.get(index)))
                .collect::<AppResult<Vec<_>>>()?,
        )),
        // Text typed into the grid for a field of another type.
        (template, _) => text.and_then(|text| typed_from_text(template, text)),
    };
    if let Some(bson) = typed {
        return Ok(bson);
    }
    let Some(text) = text else {
        // A typed value of another kind (an int for a date field): the user changed the type on purpose.
        return from_value(value);
    };
    if text.starts_with('{') || text.starts_with('[') {
        if let Some(bson) = parse_extended_json(text) {
            return Ok(match template {
                Bson::Document(_) | Bson::Array(_) => match_types(bson, template),
                _ => bson,
            });
        }
    }
    let kind = type_name(template);
    let example = match kind {
        "date" => "2025-01-02T21:36:00Z",
        "int" | "long" => "42",
        "double" | "decimal" => "4.2",
        "bool" => "true or false",
        "objectId" => "a 24-digit hex ObjectId",
        "binData" => "0x followed by hex digits",
        _ => "valid JSON",
    };
    Err(AppError::new(format!(
        "{column} holds a {kind}; enter it as {example}, or as Extended JSON (for example {{\"$date\": \"2025-01-02T21:36:00Z\"}}) to change its type."
    )))
}

/// Text as the template's type, or `None` when it does not parse.
fn typed_from_text(template: &Bson, text: &str) -> Option<Bson> {
    match template {
        Bson::String(_) => Some(Bson::String(text.to_owned())),
        Bson::Int32(_) => text.parse::<i32>().ok().map(Bson::Int32),
        Bson::Int64(_) => text.parse::<i64>().ok().map(Bson::Int64),
        Bson::Double(_) => text.parse::<f64>().ok().map(Bson::Double),
        Bson::Decimal128(_) if crate::contract::value::is_decimal_text(text) => decimal(text),
        Bson::DateTime(_) => parse_timestamptz(text).map(date_of),
        Bson::Boolean(_) => match text.to_ascii_lowercase().as_str() {
            "true" => Some(Bson::Boolean(true)),
            "false" => Some(Bson::Boolean(false)),
            _ => None,
        },
        Bson::ObjectId(_) => object_id(text).ok().map(Bson::ObjectId),
        Bson::Binary(binary) if is_uuid(binary) => uuid_bytes(text).map(|bytes| {
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes,
            })
        }),
        Bson::Binary(binary) => unhex(text, false).map(|bytes| {
            Bson::Binary(Binary {
                subtype: binary.subtype,
                bytes,
            })
        }),
        // Documents, arrays and the other types are written as Extended JSON.
        _ => None,
    }
}

/// Whether a stored field still holds what the user read (`original`): equal as typed values, or the original coerced
/// to the stored type equals it.
pub fn matches_original(stored: Option<&Bson>, original: &Value) -> bool {
    if let Some(loaded) = exact_json(original) {
        // A value the document views loaded as relaxed Extended JSON: compared in that form, so what the view showed is
        // what is checked (a missing field reads as null).
        return loaded.is_some_and(|loaded| json_equal(&relaxed_or_null(stored), &loaded));
    }
    let current = stored.map(to_value).unwrap_or(Value::Null);
    if &current == original {
        return true;
    }
    match stored {
        Some(stored) if !matches!(stored, Bson::Null | Bson::Undefined) => {
            coerce("", original, Some(stored)).is_ok_and(|coerced| &coerced == stored)
        }
        _ => false,
    }
}

/// A stored value as relaxed Extended JSON; null when missing.
pub fn relaxed_or_null(stored: Option<&Bson>) -> serde_json::Value {
    stored.map_or(serde_json::Value::Null, |stored| stored.clone().into_relaxed_extjson())
}

/// The `Value::Other` type name of an exact value from the document views: Extended JSON text (canonical or relaxed),
/// written as it is (not coerced to the stored type) and compared as loaded.
pub const EXACT_TYPE: &str = "bson";
/// The `Value::Other` type name of a field the document views remove (`$unset`).
pub const UNSET_TYPE: &str = "unset";

/// An exact value: `Value::Other { type_name: "bson", text }`.
pub fn exact(text: impl Into<String>) -> Value {
    Value::Other {
        type_name: EXACT_TYPE.to_owned(),
        text: text.into(),
    }
}

/// The marker of a removed field.
pub fn unset() -> Value {
    Value::Other {
        type_name: UNSET_TYPE.to_owned(),
        text: String::new(),
    }
}

pub fn is_unset(value: &Value) -> bool {
    matches!(value, Value::Other { type_name, .. } if type_name == UNSET_TYPE)
}

pub fn is_exact(value: &Value) -> bool {
    matches!(value, Value::Other { type_name, .. } if type_name == EXACT_TYPE)
}

/// The JSON of an exact value (`Some(None)` when its text is not JSON); `None` for any other value.
fn exact_json(value: &Value) -> Option<Option<serde_json::Value>> {
    match value {
        Value::Other { type_name, text } if type_name == EXACT_TYPE => Some(serde_json::from_str(text.trim()).ok()),
        _ => None,
    }
}

/// An exact value as BSON; `None` for any other value.
pub fn exact_bson(value: &Value) -> Option<AppResult<Bson>> {
    exact_json(value).map(|json| {
        let json = json.ok_or_else(|| AppError::new("The value is not valid Extended JSON"))?;
        Bson::try_from(json).map_err(|error| AppError::new(format!("The value is not valid Extended JSON ({error})")))
    })
}

/// JSON equality where numbers compare by value (relaxed Extended JSON writes a whole double as `10.0`, a browser sends
/// it back as `10`). Key order is not compared (the JSON maps here are sorted).
pub fn json_equal(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (left, right) {
        (J::Number(a), J::Number(b)) => a == b || a.as_f64().zip(b.as_f64()).is_some_and(|(a, b)| a == b),
        (J::Array(a), J::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_equal(a, b)),
        (J::Object(a), J::Object(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|((key_a, a), (key_b, b))| key_a == key_b && json_equal(a, b))
        }
        _ => left == right,
    }
}

/// The value at a dotted path (`address.city`, `tags.0`).
pub fn get_path<'a>(document: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut parts = path.split('.');
    let mut current = document.get(parts.next()?)?;
    for part in parts {
        current = match current {
            Bson::Document(inner) => inner.get(part)?,
            Bson::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Sets a dotted path in a new document (inserts with `address.city` columns), creating the nested documents.
pub fn set_path(document: &mut Document, path: &str, value: Bson) {
    match path.split_once('.') {
        None => {
            document.insert(path, value);
        }
        Some((head, rest)) => {
            if !matches!(document.get(head), Some(Bson::Document(_))) {
                document.insert(head, Document::new());
            }
            if let Some(Bson::Document(inner)) = document.get_mut(head) {
                set_path(inner, rest, value);
            }
        }
    }
}

/// A document as fields (top-level keys in order).
pub fn fields_of(document: &Document) -> Vec<Field> {
    document.iter().map(|(key, value)| Field::new(key, to_value(value))).collect()
}

/// Columns for a set of documents: the union of their top-level keys in order of appearance (`_id` first), each with the
/// BSON types seen (`int | string`) and the kind of the most frequent one.
pub fn columns_of(documents: &[Document]) -> Vec<ColumnInfo> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashMap<String, Vec<(&'static str, usize, &Bson)>> = HashMap::new();
    if documents.iter().any(|document| document.contains_key(ID_FIELD)) {
        order.push(ID_FIELD.to_owned());
    }
    for document in documents {
        for (key, value) in document {
            if !order.iter().any(|name| name == key) {
                order.push(key.clone());
            }
            if matches!(value, Bson::Null | Bson::Undefined) {
                continue;
            }
            let types = seen.entry(key.clone()).or_default();
            let name = type_name(value);
            match types.iter_mut().find(|(seen, _, _)| *seen == name) {
                Some(entry) => entry.1 += 1,
                None => types.push((name, 1, value)),
            }
        }
    }
    order
        .iter()
        .map(|name| {
            let types = seen.get(name).map(Vec::as_slice).unwrap_or_default();
            // The most frequent type; the first seen on a tie.
            let dominant = types
                .iter()
                .fold(None::<&(&str, usize, &Bson)>, |best, entry| match best {
                    Some(best) if best.1 >= entry.1 => Some(best),
                    _ => Some(entry),
                })
                .map(|(_, _, sample)| *sample);
            let type_text = if types.is_empty() {
                "null".to_owned()
            } else {
                types.iter().map(|(name, _, _)| *name).collect::<Vec<_>>().join(" | ")
            };
            let mut column = ColumnInfo::result(name, &type_text, dominant.map_or(ValueKind::Other, kind_of));
            column.nullable = name != ID_FIELD;
            column.hints.primary = name == ID_FIELD;
            column.hints.binary = matches!(column.kind, ValueKind::Bytes);
            column
        })
        .collect()
}

/// Rows for `columns`: a missing field is NULL.
pub fn rows_of(documents: &[Document], columns: &[ColumnInfo]) -> Vec<Vec<Value>> {
    documents
        .iter()
        .map(|document| {
            columns
                .iter()
                .map(|column| document.get(&column.name).map_or(Value::Null, to_value))
                .collect()
        })
        .collect()
}

/// A display text for a document in messages and "show query" (relaxed Extended JSON).
pub fn document_text(document: &Document) -> String {
    extended_json(&Bson::Document(document.clone()))
}

/// A regex with its options in the order the server stores them.
pub fn regex(pattern: &str, options: &str) -> Bson {
    let mut chars: Vec<char> = options.chars().collect();
    chars.sort_unstable();
    Bson::RegularExpression(Regex {
        pattern: pattern.to_owned(),
        options: chars.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{doc, Timestamp};

    fn date(text: &str) -> DateTime {
        DateTime::parse_rfc3339_str(text).unwrap()
    }

    #[test]
    fn bson_reads_as_typed_values() {
        let id = ObjectId::parse_str("65f0c0ffee0000000000abcd").unwrap();
        let document = doc! {
            "_id": id,
            "i": 5_i32,
            "l": 7_i64,
            "d": 1.5,
            "dec": "12.50".parse::<Decimal128>().unwrap(),
            "b": true,
            "at": date("2026-01-02T03:04:05.250Z"),
            "s": "x",
            "bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![0xde, 0xad] },
            "uuid": Binary { subtype: BinarySubtype::Uuid, bytes: vec![0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd, 0x38, 0x0a, 0x11] },
            "nested": { "z": 1_i32, "a": [1_i32, "two"] },
            "re": Bson::RegularExpression(Regex { pattern: "^a".into(), options: "i".into() }),
            "ts": Bson::Timestamp(Timestamp { time: 1, increment: 2 }),
            "min": Bson::MinKey,
            "nil": Bson::Null,
        };
        let get = |key: &str| to_value(document.get(key).unwrap());
        assert_eq!(get("_id"), Value::ObjectId("65f0c0ffee0000000000abcd".into()));
        assert_eq!(get("i"), Value::Int(5));
        assert_eq!(get("l"), Value::Int(7));
        assert_eq!(get("d"), Value::Float(1.5));
        assert_eq!(get("dec"), Value::Decimal("12.50".into()));
        assert_eq!(get("b"), Value::Bool(true));
        assert_eq!(get("at").display().unwrap(), "2026-01-02 03:04:05.250+00:00");
        assert_eq!(get("s"), Value::Text("x".into()));
        assert_eq!(get("bin"), Value::Bytes(vec![0xde, 0xad]));
        assert_eq!(get("uuid"), Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()));
        assert_eq!(
            get("nested"),
            Value::Document(vec![
                ("z".into(), Value::Int(1)),
                ("a".into(), Value::Array(vec![Value::Int(1), Value::Text("two".into())])),
            ])
        );
        assert!(matches!(get("re"), Value::Other { type_name, text } if type_name == "regex" && text.contains("regularExpression")));
        assert!(matches!(get("ts"), Value::Other { type_name, .. } if type_name == "timestamp"));
        assert!(matches!(get("min"), Value::Other { type_name, .. } if type_name == "minKey"));
        assert_eq!(get("nil"), Value::Null);
        // Every typed value goes back to the same BSON.
        for key in [
            "_id", "i", "l", "d", "dec", "b", "at", "s", "bin", "uuid", "nested", "re", "ts", "min",
        ] {
            let stored = document.get(key).unwrap();
            assert_eq!(&coerce(key, &to_value(stored), Some(stored)).unwrap(), stored, "{key}");
        }
    }

    #[test]
    fn values_without_a_template_take_their_natural_type() {
        assert_eq!(from_value(&Value::Int(5)).unwrap(), Bson::Int32(5));
        assert_eq!(from_value(&Value::Int(1 << 40)).unwrap(), Bson::Int64(1 << 40));
        assert_eq!(
            from_value(&Value::Decimal("9.99".into())).unwrap(),
            Bson::Decimal128("9.99".parse().unwrap())
        );
        assert!(matches!(
            from_value(&Value::Json(r#"{"$date": "2025-01-02T21:36:00Z"}"#.into())).unwrap(),
            Bson::DateTime(_)
        ));
        assert!(
            matches!(from_value(&Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into())).unwrap(), Bson::Binary(b) if b.subtype == BinarySubtype::Uuid)
        );
        assert_eq!(
            key_to_bson("_id", &Value::Text("65f0c0ffee0000000000abcd".into())).unwrap(),
            Bson::ObjectId(ObjectId::parse_str("65f0c0ffee0000000000abcd").unwrap())
        );
        assert_eq!(
            key_to_bson("name", &Value::Text("65f0c0ffee0000000000abcd".into())).unwrap(),
            Bson::String("65f0c0ffee0000000000abcd".into())
        );
    }

    #[test]
    fn edits_keep_the_stored_bson_type() {
        let at = date("2025-01-02T21:36:00Z");
        let shown = to_value(&Bson::DateTime(at));
        assert_eq!(coerce("signedUpAt", &shown, Some(&Bson::DateTime(at))).unwrap(), Bson::DateTime(at));
        let typed = Value::Text("2026-03-04T05:06:07.123Z".into());
        assert!(matches!(
            coerce("signedUpAt", &typed, Some(&Bson::DateTime(at))).unwrap(),
            Bson::DateTime(_)
        ));
        assert_eq!(coerce("n", &Value::Int(5), Some(&Bson::Int64(1))).unwrap(), Bson::Int64(5));
        assert_eq!(coerce("n", &Value::Int(5), Some(&Bson::Int32(1))).unwrap(), Bson::Int32(5));
        assert_eq!(
            coerce("n", &Value::Text("5".into()), Some(&Bson::Int32(1))).unwrap(),
            Bson::Int32(5)
        );
        assert_eq!(coerce("x", &Value::Int(2), Some(&Bson::Double(1.5))).unwrap(), Bson::Double(2.0));
        assert_eq!(
            coerce("ok", &Value::Text("false".into()), Some(&Bson::Boolean(true))).unwrap(),
            Bson::Boolean(false)
        );
        // A string field stays a string even when the text looks like a number.
        assert_eq!(
            coerce("zip", &Value::Text("01234".into()), Some(&Bson::String("x".into()))).unwrap(),
            Bson::String("01234".into())
        );
        let price: Decimal128 = "9.99".parse().unwrap();
        assert_eq!(
            coerce("price", &Value::Decimal("10.50".into()), Some(&Bson::Decimal128(price))).unwrap(),
            Bson::Decimal128("10.50".parse().unwrap())
        );
        assert_eq!(
            coerce("price", &Value::Int(3), Some(&Bson::Decimal128(price))).unwrap(),
            Bson::Decimal128("3".parse().unwrap())
        );
        let id = ObjectId::new();
        assert_eq!(
            coerce("ref", &Value::Text(id.to_hex()), Some(&Bson::ObjectId(ObjectId::new()))).unwrap(),
            Bson::ObjectId(id)
        );
        let binary = Bson::Binary(Binary {
            subtype: BinarySubtype::UserDefined(0x80),
            bytes: vec![1],
        });
        assert_eq!(
            coerce("blob", &Value::Bytes(vec![2, 3]), Some(&binary)).unwrap(),
            Bson::Binary(Binary {
                subtype: BinarySubtype::UserDefined(0x80),
                bytes: vec![2, 3]
            })
        );
        assert_eq!(coerce("gone", &Value::Null, Some(&Bson::Int32(1))).unwrap(), Bson::Null);
    }

    #[test]
    fn json_text_for_documents_and_arrays_keeps_number_types() {
        let at = date("2025-01-02T21:36:00Z");
        let nested = Bson::Document(doc! { "at": at, "n": 3_i64, "x": 2.0, "tags": ["a", "b"] });
        let shown = to_value(&nested).display().unwrap();
        assert_eq!(coerce("meta", &Value::Json(shown.clone()), Some(&nested)).unwrap(), nested);
        assert_eq!(coerce("meta", &Value::Text(shown), Some(&nested)).unwrap(), nested);
        let array = Bson::Array(vec![Bson::Int64(1), Bson::Int64(2)]);
        assert_eq!(
            coerce("tags", &Value::Json("[1, 2, 3]".into()), Some(&array)).unwrap(),
            Bson::Array(vec![Bson::Int64(1), Bson::Int64(2), Bson::Int32(3)])
        );
        let edited = Value::Document(vec![("n".into(), Value::Int(4)), ("x".into(), Value::Int(5))]);
        assert_eq!(
            coerce("meta", &edited, Some(&nested)).unwrap(),
            Bson::Document(doc! { "n": 4_i64, "x": 5.0 })
        );
    }

    #[test]
    fn wrong_text_for_a_typed_field_is_refused_with_a_hint() {
        let error = coerce(
            "signedUpAt",
            &Value::Text("tomorrow".into()),
            Some(&Bson::DateTime(DateTime::now())),
        )
        .unwrap_err();
        assert!(error.to_string().contains("signedUpAt holds a date"), "{error}");
        // Explicit Extended JSON changes the type on purpose.
        assert_eq!(
            coerce("n", &Value::Text(r#"{"$numberDouble": "1.5"}"#.into()), Some(&Bson::Int32(1))).unwrap(),
            Bson::Double(1.5)
        );
    }

    #[test]
    fn originals_match_across_number_types() {
        assert!(matches_original(Some(&Bson::Int32(10)), &Value::Int(10)));
        assert!(matches_original(Some(&Bson::Double(10.0)), &Value::Int(10)));
        assert!(matches_original(Some(&Bson::String("a".into())), &Value::Text("a".into())));
        assert!(!matches_original(Some(&Bson::String("b".into())), &Value::Text("a".into())));
        assert!(matches_original(None, &Value::Null));
        assert!(!matches_original(None, &Value::Text("a".into())));
        assert!(matches_original(
            Some(&Bson::Decimal128("1.25".parse().unwrap())),
            &Value::Decimal("1.25".into())
        ));
    }

    #[test]
    fn paths_and_columns() {
        let document = doc! { "_id": 1, "a": { "b": [10, 20] } };
        assert_eq!(get_path(&document, "a.b.1"), Some(&Bson::Int32(20)));
        assert_eq!(get_path(&document, "a.c"), None);
        let mut built = Document::new();
        set_path(&mut built, "address.city", Bson::String("Hue".into()));
        set_path(&mut built, "address.zip", Bson::String("1".into()));
        assert_eq!(built, doc! { "address": { "city": "Hue", "zip": "1" } });
        let documents = [
            doc! { "x": 1, "_id": 1, "at": DateTime::now(), "mixed": 1 },
            doc! { "_id": 2, "mixed": "x", "at": Bson::Null, "mixed2": "y" },
            doc! { "_id": 3, "mixed": "z" },
        ];
        let columns = columns_of(&documents);
        let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "x", "at", "mixed", "mixed2"]);
        let types: Vec<&str> = columns.iter().map(|c| c.type_name.as_str()).collect();
        assert_eq!(types, ["int", "int", "date", "int | string", "string"]);
        assert_eq!(columns[3].kind, ValueKind::Text);
        assert!(columns[0].hints.primary);
        let rows = rows_of(&documents, &columns);
        assert_eq!(
            rows[2],
            vec![Value::Int(3), Value::Null, Value::Null, Value::Text("z".into()), Value::Null]
        );
    }

    #[test]
    fn commands_parse_as_extended_json_in_key_order() {
        let command = parse_document(
            r#"{ "find": "c", "filter": { "_id": { "$oid": "65f0c0ffee0000000000abcd" }, "at": { "$gt": { "$date": "2025-01-01T00:00:00Z" } }, "p": { "$numberDecimal": "1.10" } } }"#,
        )
        .unwrap();
        assert_eq!(command.keys().next().map(String::as_str), Some("find"));
        let filter = command.get_document("filter").unwrap();
        assert!(matches!(filter.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(filter.get_document("at").unwrap().get("$gt"), Some(Bson::DateTime(_))));
        assert!(matches!(filter.get("p"), Some(Bson::Decimal128(_))));
        assert!(parse_document("[1]").is_err());
        assert!(parse_document(r#"{ "find": "#).is_err());
    }
}
