//! Typed values for the grid: columns from the mapping (top-level fields in mapping order), `_source` values typed by
//! their field type, and contract values written back as the JSON the mapping expects.
//!
//! A field may hold several values (every Elasticsearch field is multi-valued), so a JSON array in a scalar field
//! becomes an array of typed values. Values the mapping does not explain keep their JSON shape, and a source value that
//! does not fit its field type (a number sent as a string) keeps its own type, so writing it back does not change it.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde_json::{Map, Number, Value as Json};

use crate::{
    contract::{value::format_float, ColumnHints, ColumnInfo, Value, ValueKind},
    error::{AppError, AppResult},
};

/// One grid column: a top-level field of the mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldColumn {
    pub name: String,
    /// The mapping type (`keyword`, `long`, `object` for plain objects).
    pub field_type: String,
    pub kind: ValueKind,
    /// The field to use for exact matches, wildcards and sorting: the field itself, or its `keyword` sub-field for
    /// `text`; `None` when there is none.
    pub exact: Option<String>,
}

impl FieldColumn {
    pub fn info(&self, primary: bool) -> ColumnInfo {
        let mut info = ColumnInfo::result(&self.name, &self.field_type, self.kind.clone());
        info.hints = ColumnHints {
            primary,
            ..ColumnHints::default()
        };
        info
    }

    pub fn is_text(&self) -> bool {
        matches!(
            self.field_type.as_str(),
            "text" | "match_only_text" | "search_as_you_type" | "keyword" | "constant_keyword" | "wildcard"
        )
    }
}

/// The neutral kind of a mapping type.
pub fn kind_of(field_type: &str) -> ValueKind {
    match field_type {
        "long" | "integer" | "short" | "byte" | "unsigned_long" | "token_count" => ValueKind::Int,
        "double" | "float" | "half_float" => ValueKind::Float,
        "scaled_float" => ValueKind::Decimal,
        "boolean" => ValueKind::Bool,
        // Mapping types, plus the names ES|QL and SQL give date columns.
        "date" | "date_nanos" | "datetime" | "timestamp" => ValueKind::TimestampTz,
        "object" | "flattened" => ValueKind::Document,
        "nested" => ValueKind::Array(Box::new(ValueKind::Document)),
        "dense_vector" => ValueKind::Array(Box::new(ValueKind::Float)),
        "keyword" | "constant_keyword" | "wildcard" | "text" | "match_only_text" | "search_as_you_type" | "completion" | "version"
        | "ip" | "binary" | "semantic_text" => ValueKind::Text,
        _ => ValueKind::Json,
    }
}

/// The top-level fields of a mapping (`{ "properties": … }`), in mapping order.
pub fn columns_from_mapping(mappings: &Json) -> Vec<FieldColumn> {
    let Some(properties) = mappings["properties"].as_object() else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, definition)| {
            let field_type = definition["type"].as_str().unwrap_or("object").to_owned();
            let exact = match field_type.as_str() {
                "keyword" | "constant_keyword" | "wildcard" | "long" | "integer" | "short" | "byte" | "unsigned_long" | "double"
                | "float" | "half_float" | "scaled_float" | "boolean" | "date" | "date_nanos" | "ip" | "version" => Some(name.clone()),
                "text" | "match_only_text" => definition["fields"].as_object().and_then(|fields| {
                    fields
                        .iter()
                        .find(|(_, sub)| sub["type"] == "keyword")
                        .map(|(sub_name, _)| format!("{name}.{sub_name}"))
                }),
                _ => None,
            };
            FieldColumn {
                name: name.clone(),
                kind: kind_of(&field_type),
                field_type,
                exact,
            }
        })
        .collect()
}

/// A JSON value without a mapping: scalars, arrays and objects as their contract counterparts.
pub fn generic(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => number(n),
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(items) => Value::Array(items.iter().map(generic).collect()),
        Json::Object(map) => Value::Document(map.iter().map(|(k, v)| (k.clone(), generic(v))).collect()),
    }
}

fn number(n: &Number) -> Value {
    match (n.as_i64(), n.as_f64()) {
        (Some(i), _) => Value::Int(i),
        (None, Some(_)) if n.is_u64() => Value::Decimal(n.to_string()),
        (None, Some(f)) => Value::Float(f),
        _ => Value::Decimal(n.to_string()),
    }
}

fn timestamp(text: &str) -> Option<Value> {
    if let Ok(value) = DateTime::parse_from_rfc3339(text) {
        return Some(Value::TimestampTz(value));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(text, format) {
            return Some(Value::Timestamp(value));
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().map(Value::Date)
}

/// A `_source` value typed by its field's kind.
pub fn to_value(json: &Json, kind: &ValueKind) -> Value {
    match (json, kind) {
        (Json::Null, _) => Value::Null,
        (Json::Array(items), ValueKind::Array(inner)) => Value::Array(items.iter().map(|item| to_value(item, inner)).collect()),
        // Geo points written as `[lon, lat]` stay one JSON value; other arrays are lists of JSON values.
        (Json::Array(items), ValueKind::Json) if items.iter().all(Json::is_number) => Value::Json(json.to_string()),
        (Json::Array(items), ValueKind::Json) => Value::Array(items.iter().map(|item| Value::Json(item.to_string())).collect()),
        (Json::Array(items), _) => Value::Array(items.iter().map(|item| to_value(item, kind)).collect()),
        (_, ValueKind::Json) => Value::Json(json.to_string()),
        (Json::Number(n), ValueKind::Decimal) => Value::Decimal(n.to_string()),
        (Json::String(s), ValueKind::Int) => s.trim().parse().map(Value::Int).unwrap_or_else(|_| Value::Text(s.clone())),
        (Json::String(s), ValueKind::Float) => s.trim().parse().map(Value::Float).unwrap_or_else(|_| Value::Text(s.clone())),
        (Json::String(s), ValueKind::Bool) => match s.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::Text(s.clone()),
        },
        (Json::String(s), ValueKind::TimestampTz) => timestamp(s).unwrap_or_else(|| Value::Text(s.clone())),
        (other, _) => generic(other),
    }
}

/// A contract value as the JSON to store in a field of `kind`.
pub fn to_json(value: &Value, kind: &ValueKind) -> AppResult<Json> {
    let invalid = |what: &str| AppError::new(format!("'{what}' is not a valid value for this field."));
    Ok(match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Float(f) => Number::from_f64(*f).map(Json::Number).ok_or_else(|| invalid(&format_float(*f)))?,
        Value::Decimal(text) | Value::Text(text) => match kind {
            ValueKind::Int => text.trim().parse::<i64>().map(Json::from).map_err(|_| invalid(text))?,
            ValueKind::Float | ValueKind::Decimal => text
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Json::Number)
                .ok_or_else(|| invalid(text))?,
            ValueKind::Bool => match text.trim() {
                "true" => Json::Bool(true),
                "false" => Json::Bool(false),
                _ => return Err(invalid(text)),
            },
            ValueKind::Json | ValueKind::Document | ValueKind::Array(_) => serde_json::from_str(text).unwrap_or(Json::String(text.clone())),
            _ => Json::String(text.clone()),
        },
        Value::Json(text) => serde_json::from_str(text).map_err(|_| invalid(text))?,
        Value::Array(items) => {
            let inner = match kind {
                ValueKind::Array(inner) => inner.as_ref().clone(),
                other => other.clone(),
            };
            Json::Array(items.iter().map(|item| to_json(item, &inner)).collect::<AppResult<_>>()?)
        }
        Value::Document(fields) => {
            let mut map = Map::new();
            for (key, inner) in fields {
                map.insert(key.clone(), to_json(inner, &ValueKind::Json)?);
            }
            Json::Object(map)
        }
        Value::Bytes(bytes) => Json::String(STANDARD.encode(bytes)),
        Value::Date(date) => Json::String(date.format("%Y-%m-%d").to_string()),
        Value::Timestamp(ts) => Json::String(ts.format("%Y-%m-%dT%H:%M:%S%.f").to_string()),
        Value::TimestampTz(ts) => Json::String(ts.to_rfc3339()),
        other => other.to_json(),
    })
}

/// A filter value typed for a field: numbers and booleans as JSON scalars, everything else as a string.
pub fn filter_value(text: &str, kind: &ValueKind) -> AppResult<Json> {
    match kind {
        ValueKind::Int | ValueKind::Float | ValueKind::Decimal | ValueKind::Bool => to_json(&Value::Text(text.to_owned()), kind),
        _ => Ok(Json::String(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn columns_follow_the_mapping() {
        let mappings = json!({
            "_meta": { "fosta": { "key": "id" } },
            "properties": {
                "active": { "type": "boolean" },
                "born": { "type": "date" },
                "id": { "type": "long" },
                "name": { "type": "text", "fields": { "raw": { "type": "keyword" } } },
                "note": { "type": "keyword" },
                "price": { "type": "scaled_float", "scaling_factor": 100 },
                "supplier": { "properties": { "name": { "type": "keyword" } } },
                "variants": { "type": "nested", "properties": { "sku": { "type": "keyword" } } },
                "location": { "type": "geo_point" }
            }
        });
        let columns = columns_from_mapping(&mappings);
        let find = |name: &str| columns.iter().find(|c| c.name == name).unwrap();
        assert_eq!(find("id").kind, ValueKind::Int);
        assert_eq!(find("price").kind, ValueKind::Decimal);
        assert_eq!(find("name").exact.as_deref(), Some("name.raw"));
        assert_eq!(find("note").exact.as_deref(), Some("note"));
        assert_eq!(find("supplier").kind, ValueKind::Document);
        assert_eq!(find("variants").kind, ValueKind::Array(Box::new(ValueKind::Document)));
        assert_eq!(find("location").kind, ValueKind::Json);
        assert!(find("supplier").exact.is_none());
    }

    #[test]
    fn source_values_are_typed_and_written_back_unchanged() {
        assert_eq!(to_value(&json!(5), &ValueKind::Int), Value::Int(5));
        assert_eq!(to_value(&json!("5"), &ValueKind::Int), Value::Int(5));
        assert_eq!(to_value(&json!(12.5), &ValueKind::Decimal), Value::Decimal("12.5".into()));
        assert_eq!(
            to_value(&json!([1, 2]), &ValueKind::Int),
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
        let stamp = to_value(&json!("2026-01-01T08:00:00.250Z"), &ValueKind::TimestampTz);
        assert!(matches!(stamp, Value::TimestampTz(_)));
        assert_eq!(
            to_json(&stamp, &ValueKind::TimestampTz).unwrap(),
            json!("2026-01-01T08:00:00.250+00:00")
        );
        assert_eq!(
            to_value(&json!("2026-01-02"), &ValueKind::TimestampTz),
            Value::Date(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
        );
        assert_eq!(
            to_value(&json!(1_700_000_000_000_i64), &ValueKind::TimestampTz),
            Value::Int(1_700_000_000_000)
        );
        assert_eq!(to_value(&json!("soon"), &ValueKind::TimestampTz), Value::Text("soon".into()));
        assert_eq!(
            to_value(&json!([-71.3, 41.1]), &ValueKind::Json),
            Value::Json("[-71.3,41.1]".into())
        );
        assert_eq!(
            to_value(&json!({ "lat": 1, "lon": 2 }), &ValueKind::Json),
            Value::Json("{\"lat\":1,\"lon\":2}".into())
        );
        let doc = to_value(&json!({ "name": "acme", "n": 1 }), &ValueKind::Document);
        assert_eq!(to_json(&doc, &ValueKind::Document).unwrap(), json!({ "name": "acme", "n": 1 }));
        assert_eq!(to_json(&Value::Decimal("9.99".into()), &ValueKind::Decimal).unwrap(), json!(9.99));
        assert_eq!(to_json(&Value::Text("42".into()), &ValueKind::Int).unwrap(), json!(42));
        assert!(to_json(&Value::Text("x".into()), &ValueKind::Int).is_err());
        assert_eq!(to_json(&Value::Null, &ValueKind::Text).unwrap(), Json::Null);
        assert_eq!(
            to_json(&Value::Json("{\"a\":1}".into()), &ValueKind::Json).unwrap(),
            json!({ "a": 1 })
        );
    }

    #[test]
    fn filter_values_follow_the_field_kind() {
        assert_eq!(filter_value("5", &ValueKind::Int).unwrap(), json!(5));
        assert_eq!(filter_value("true", &ValueKind::Bool).unwrap(), json!(true));
        assert_eq!(filter_value("5", &ValueKind::Text).unwrap(), json!("5"));
        assert!(filter_value("five", &ValueKind::Int).is_err());
    }
}
