//! ClickHouse type mapping. Results come as `JSONCompactEachRowWithNamesAndTypes` with 64-bit integers, decimals and non-finite
//! floats quoted, so every value arrives without loss and with its type name; parameters go back as `{pN:Type}` query
//! parameters in ClickHouse's text format.

use crate::engines::common::value::{normalize_uuid, parse_time, parse_timestamp};
use crate::engines::common::{Value, ValueKind};
use chrono::NaiveDate;
use serde_json::Value as Json;

/// Strips `Nullable(…)` and `LowCardinality(…)` wrappers.
pub fn base_type(ty: &str) -> &str {
    let mut t = ty.trim();
    loop {
        if let Some(inner) = t.strip_prefix("Nullable(").and_then(|s| s.strip_suffix(')')) {
            t = inner;
        } else if let Some(inner) = t.strip_prefix("LowCardinality(").and_then(|s| s.strip_suffix(')')) {
            t = inner;
        } else {
            return t;
        }
    }
}

/// The name of a type without its arguments (`DateTime64(3, 'UTC')` → `DateTime64`).
fn head(ty: &str) -> &str {
    ty.split('(').next().unwrap_or(ty).trim()
}

/// The arguments of `Name(a, b)` split at top-level commas.
pub fn args(ty: &str) -> Vec<&str> {
    let Some(open) = ty.find('(') else { return Vec::new() };
    let inner = &ty[open + 1..ty.len().saturating_sub(1)];
    let mut out = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, false);
    for (i, c) in inner.char_indices() {
        match c {
            '\'' => quote = !quote,
            '(' if !quote => depth += 1,
            ')' if !quote => depth -= 1,
            ',' if !quote && depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = inner[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

pub fn is_nullable(ty: &str) -> bool {
    let t = ty.trim();
    t.starts_with("Nullable(") || t.strip_prefix("LowCardinality(").is_some_and(|s| s.starts_with("Nullable("))
}

pub fn kind_of(ty: &str) -> ValueKind {
    let t = base_type(ty);
    match head(t) {
        "Bool" => ValueKind::Bool,
        "Int8" | "Int16" | "Int32" | "Int64" | "Int128" | "Int256" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "UInt128" | "UInt256" => {
            ValueKind::Int
        }
        "Float32" | "Float64" | "BFloat16" => ValueKind::Float,
        "Decimal" | "Decimal32" | "Decimal64" | "Decimal128" | "Decimal256" => ValueKind::Decimal,
        "String" | "FixedString" | "Enum8" | "Enum16" | "Enum" => ValueKind::Text,
        "Date" | "Date32" => ValueKind::Date,
        "Time" | "Time64" => ValueKind::Time,
        "DateTime" | "DateTime64" => ValueKind::Timestamp,
        "UUID" => ValueKind::Uuid,
        "JSON" | "Object" | "Map" => ValueKind::Json,
        "Array" => ValueKind::Array(Box::new(args(t).first().map(|a| kind_of(a)).unwrap_or(ValueKind::Other))),
        h if h.starts_with("Interval") => ValueKind::Interval,
        _ => ValueKind::Other,
    }
}

/// Converts one JSON cell of a column of type `ty`.
pub fn read(v: &Json, ty: &str) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    let t = base_type(ty);
    let other = || Value::Other {
        type_name: t.to_owned(),
        text: json_text(v),
    };
    match kind_of(t) {
        ValueKind::Bool => v.as_bool().map(Value::Bool).unwrap_or_else(other),
        ValueKind::Int => match v {
            Json::Number(n) => n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Decimal(n.to_string())),
            Json::String(s) => s.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::Decimal(s.clone())),
            _ => other(),
        },
        ValueKind::Float => match v {
            Json::Number(n) => n.as_f64().map(Value::Float).unwrap_or_else(other),
            Json::String(s) => match s.as_str() {
                "nan" | "-nan" => Value::Float(f64::NAN),
                "inf" | "+inf" => Value::Float(f64::INFINITY),
                "-inf" => Value::Float(f64::NEG_INFINITY),
                _ => s.parse().map(Value::Float).unwrap_or_else(|_| other()),
            },
            _ => other(),
        },
        ValueKind::Decimal => Value::Decimal(json_text(v)),
        ValueKind::Text => match v {
            Json::String(s) if head(t) == "FixedString" => Value::Text(s.trim_end_matches('\0').to_owned()),
            Json::String(s) => Value::Text(s.clone()),
            _ => other(),
        },
        ValueKind::Date => v
            .as_str()
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .map(Value::Date)
            .unwrap_or_else(other),
        ValueKind::Time => v.as_str().and_then(parse_time).map(Value::Time).unwrap_or_else(other),
        ValueKind::Timestamp => v.as_str().and_then(parse_timestamp).map(Value::Timestamp).unwrap_or_else(other),
        ValueKind::Uuid => v.as_str().and_then(normalize_uuid).map(Value::Uuid).unwrap_or_else(other),
        ValueKind::Json => match v {
            Json::String(s) => Value::Json(s.clone()),
            other_json => Value::Json(other_json.to_string()),
        },
        ValueKind::Interval => Value::Interval(json_text(v)),
        ValueKind::Array(_) => {
            let elem = args(t).first().copied().unwrap_or("String").to_owned();
            match v {
                Json::Array(items) => Value::Array(items.iter().map(|i| read(i, &elem)).collect()),
                _ => other(),
            }
        }
        ValueKind::Bytes | ValueKind::TimestampTz | ValueKind::Other => other(),
    }
}

fn json_text(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The text form ClickHouse parses for a query parameter (`param_pN`). Strings are passed raw (escaped for the TSV-like
/// parameter format); arrays use ClickHouse literals.
pub fn to_param_text(v: &Value) -> String {
    match v {
        Value::Null => "\\N".to_owned(),
        Value::Text(s) => escape_param(s),
        Value::Bytes(b) => escape_param(&String::from_utf8_lossy(b)),
        Value::Float(f) if f.is_nan() => "nan".to_owned(),
        Value::Float(f) if f.is_infinite() => if *f > 0.0 { "inf" } else { "-inf" }.to_owned(),
        Value::TimestampTz(t) => t.naive_utc().format("%Y-%m-%d %H:%M:%S%.f").to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(literal).collect::<Vec<_>>().join(",")),
        other => escape_param(&other.display().unwrap_or_default()),
    }
}

/// A ClickHouse literal for array elements.
fn literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Int(_) | Value::Float(_) | Value::Decimal(_) => v.display().unwrap_or_default(),
        Value::Array(items) => format!("[{}]", items.iter().map(literal).collect::<Vec<_>>().join(",")),
        other => {
            let s = other.display().unwrap_or_default();
            format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
        }
    }
}

fn escape_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwraps_and_splits_types() {
        assert_eq!(base_type("LowCardinality(Nullable(String))"), "String");
        assert_eq!(
            args("Map(String, Array(Tuple(Int32, String)))"),
            vec!["String", "Array(Tuple(Int32, String))"]
        );
        assert_eq!(args("Enum8('a,b' = 1, 'c' = 2)"), vec!["'a,b' = 1", "'c' = 2"]);
        assert!(is_nullable("LowCardinality(Nullable(String))"));
        assert_eq!(kind_of("Nullable(DateTime64(6, 'Asia/Ho_Chi_Minh'))"), ValueKind::Timestamp);
        assert_eq!(kind_of("Array(Nullable(String))"), ValueKind::Array(Box::new(ValueKind::Text)));
        assert_eq!(kind_of("IPv4"), ValueKind::Other);
    }

    #[test]
    fn reads_json_cells() {
        assert_eq!(read(&json!("9223372036854775807"), "Int64"), Value::Int(i64::MAX));
        assert_eq!(
            read(&json!("18446744073709551615"), "UInt64"),
            Value::Decimal("18446744073709551615".into())
        );
        assert_eq!(read(&json!("12.500000"), "Decimal(20, 6)"), Value::Decimal("12.500000".into()));
        assert_eq!(read(&json!("inf"), "Float64"), Value::Float(f64::INFINITY));
        assert_eq!(read(&json!("ab\u{0}\u{0}"), "FixedString(4)"), Value::Text("ab".into()));
        assert_eq!(
            read(&json!("2026-02-28 13:45:30.123456"), "DateTime64(6)").display().unwrap(),
            "2026-02-28 13:45:30.123456"
        );
        assert_eq!(
            read(&json!([1, null]), "Array(Nullable(Int32))"),
            Value::Array(vec![Value::Int(1), Value::Null])
        );
        assert_eq!(read(&json!({"k": 1}), "Map(String, UInt16)"), Value::Json("{\"k\":1}".into()));
        assert_eq!(
            read(&json!([7, "seven"]), "Tuple(Int32, String)").display().unwrap(),
            "[7,\"seven\"]"
        );
        assert_eq!(read(&json!(null), "Nullable(String)"), Value::Null);
    }

    #[test]
    fn param_text() {
        assert_eq!(to_param_text(&Value::Text("a\tb\\c".into())), "a\\tb\\\\c");
        assert_eq!(
            to_param_text(&Value::Array(vec![Value::Text("it's".into()), Value::Null])),
            "['it\\'s',NULL]"
        );
        assert_eq!(to_param_text(&Value::Null), "\\N");
    }
}
