//! SQLite type mapping. SQLite stores five storage classes whatever the declared type, so a cell is read by its storage class and
//! then interpreted with the column's declared type (a TEXT cell in a DATE column becomes a `Date` when it parses as one).

use crate::engines::common::value::{normalize_uuid, parse_time, parse_timestamp, parse_timestamptz, uuid_from_bytes};
use crate::engines::common::{Value, ValueKind};
use chrono::NaiveDate;
use rusqlite::types::{Value as Sql, ValueRef};

/// The neutral kind of a declared type: SQLite's affinity rules (section 3.1 of the datatype docs) refined by common names.
pub fn kind_of(decl: &str) -> ValueKind {
    let t = decl.to_ascii_uppercase();
    if t.contains("BOOL") {
        ValueKind::Bool
    } else if t.contains("INT") {
        ValueKind::Int
    } else if t.contains("UUID") || t.contains("GUID") {
        ValueKind::Uuid
    } else if t.contains("JSON") {
        ValueKind::Json
    } else if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") {
        ValueKind::Text
    } else if t.contains("BLOB") || t.contains("BINARY") {
        ValueKind::Bytes
    } else if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
        ValueKind::Float
    } else if t.contains("DATETIME") || t.contains("TIMESTAMP") {
        if t.contains("TZ") || t.contains("ZONE") {
            ValueKind::TimestampTz
        } else {
            ValueKind::Timestamp
        }
    } else if t.contains("DATE") {
        ValueKind::Date
    } else if t.contains("TIME") {
        ValueKind::Time
    } else if t.contains("DEC") || t.contains("NUM") {
        ValueKind::Decimal
    } else {
        // No declared type (BLOB affinity) or something unknown: show what is stored.
        ValueKind::Other
    }
}

/// Reads one cell.
pub fn read(v: ValueRef<'_>, kind: &ValueKind) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => match kind {
            ValueKind::Bool if i == 0 || i == 1 => Value::Bool(i == 1),
            _ => Value::Int(i),
        },
        ValueRef::Real(f) => match kind {
            ValueKind::Decimal => Value::Decimal(crate::engines::common::value::format_float(f)),
            _ => Value::Float(f),
        },
        ValueRef::Text(bytes) => {
            let s = String::from_utf8_lossy(bytes).into_owned();
            interpret_text(s, kind)
        }
        ValueRef::Blob(b) => match kind {
            ValueKind::Uuid if b.len() == 16 => Value::Uuid(uuid_from_bytes(b)),
            _ => Value::Bytes(b.to_vec()),
        },
    }
}

fn interpret_text(s: String, kind: &ValueKind) -> Value {
    let parsed = match kind {
        ValueKind::Date => NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok().map(Value::Date),
        ValueKind::Time => parse_time(&s).map(Value::Time),
        ValueKind::Timestamp => parse_timestamp(&s).map(Value::Timestamp),
        ValueKind::TimestampTz => parse_timestamptz(&s).map(Value::TimestampTz),
        ValueKind::Uuid => normalize_uuid(&s).map(Value::Uuid),
        ValueKind::Json => serde_json::from_str::<serde_json::Value>(&s).ok().map(|_| Value::Json(s.clone())),
        ValueKind::Decimal if crate::engines::common::value::is_decimal_text(&s) => Some(Value::Decimal(s.clone())),
        _ => None,
    };
    parsed.unwrap_or(Value::Text(s))
}

/// Converts a typed value to what is stored for a column of `kind`. Temporals are stored as ISO 8601 text (what SQLite's date
/// functions read), booleans as 0/1, decimals as integers or reals when exact and as text otherwise.
pub fn to_sql(v: &Value, kind: &ValueKind) -> Sql {
    match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Int(i) => Sql::Integer(*i),
        Value::Float(f) => Sql::Real(*f),
        Value::Decimal(s) => {
            if matches!(kind, ValueKind::Decimal | ValueKind::Int | ValueKind::Float | ValueKind::Other) {
                if let Ok(i) = s.parse::<i64>() {
                    return Sql::Integer(i);
                }
                if let Ok(f) = s.parse::<f64>() {
                    if crate::engines::common::value::format_float(f) == s.trim_start_matches('+') {
                        return Sql::Real(f);
                    }
                }
            }
            Sql::Text(s.clone())
        }
        Value::Bytes(b) => Sql::Blob(b.clone()),
        Value::Text(s) => Sql::Text(s.clone()),
        Value::Array(_) => Sql::Text(v.display().unwrap_or_default()),
        Value::Json(s) => Sql::Text(s.clone()),
        Value::Other { text, .. } => {
            // Untyped column: store integers and reals as numbers so comparisons behave.
            if let Ok(i) = text.parse::<i64>() {
                Sql::Integer(i)
            } else if let Ok(f) = text.parse::<f64>() {
                Sql::Real(f)
            } else {
                Sql::Text(text.clone())
            }
        }
        other => Sql::Text(other.display().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_map_to_kinds() {
        let cases = [
            ("INTEGER", ValueKind::Int),
            ("BIGINT", ValueKind::Int),
            ("BOOLEAN", ValueKind::Bool),
            ("VARCHAR(50)", ValueKind::Text),
            ("BLOB", ValueKind::Bytes),
            ("DOUBLE PRECISION", ValueKind::Float),
            ("NUMERIC(12,2)", ValueKind::Decimal),
            ("DATETIME", ValueKind::Timestamp),
            ("DATE", ValueKind::Date),
            ("TIME", ValueKind::Time),
            ("JSON", ValueKind::Json),
            ("UUID", ValueKind::Uuid),
            ("", ValueKind::Other),
        ];
        for (decl, kind) in cases {
            assert_eq!(kind_of(decl), kind, "{decl}");
        }
    }

    #[test]
    fn reads_by_storage_class_and_declared_type() {
        assert_eq!(read(ValueRef::Integer(1), &ValueKind::Bool), Value::Bool(true));
        assert_eq!(
            read(ValueRef::Text(b"2026-02-28"), &ValueKind::Date).display().unwrap(),
            "2026-02-28"
        );
        assert_eq!(
            read(ValueRef::Text(b"not a date"), &ValueKind::Date),
            Value::Text("not a date".into())
        );
        assert_eq!(read(ValueRef::Real(1.25), &ValueKind::Decimal), Value::Decimal("1.25".into()));
        assert_eq!(
            to_sql(&Value::Decimal("12.50".into()), &ValueKind::Decimal),
            Sql::Text("12.50".into())
        );
        assert_eq!(to_sql(&Value::Decimal("12.5".into()), &ValueKind::Decimal), Sql::Real(12.5));
    }
}
