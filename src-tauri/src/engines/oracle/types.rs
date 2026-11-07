//! Oracle type mapping: catalog type names and ODPI-C column types to neutral kinds, cells to [`Value`], and values to bind
//! parameters.

use crate::engines::common::{Value, ValueKind};
use chrono::{DateTime, FixedOffset, NaiveDateTime};
use oracle::sql_type::{IntervalDS, IntervalYM, OracleType, ToSql};
use oracle::SqlValue;

/// The neutral kind of a catalog type (`ALL_TAB_COLUMNS.DATA_TYPE` with precision and scale).
pub fn kind_by_name(data_type: &str, precision: Option<i64>, scale: Option<i64>) -> ValueKind {
    let t = data_type.to_ascii_uppercase();
    match t.as_str() {
        "NUMBER" | "INTEGER" => match (precision, scale) {
            (Some(p), Some(0)) if p <= 18 => ValueKind::Int,
            (None, Some(0)) => ValueKind::Int,
            _ => ValueKind::Decimal,
        },
        "FLOAT" => ValueKind::Decimal,
        "BINARY_FLOAT" | "BINARY_DOUBLE" => ValueKind::Float,
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" | "CLOB" | "NCLOB" | "LONG" | "ROWID" | "UROWID" => ValueKind::Text,
        "RAW" | "LONG RAW" | "BLOB" => ValueKind::Bytes,
        "DATE" => ValueKind::Timestamp,
        "BOOLEAN" => ValueKind::Bool,
        "JSON" => ValueKind::Json,
        _ if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") => ValueKind::TimestampTz,
        _ if t.starts_with("TIMESTAMP") => ValueKind::Timestamp,
        _ if t.starts_with("INTERVAL") => ValueKind::Interval,
        _ => ValueKind::Other,
    }
}

/// The neutral kind of a result column.
pub fn kind_of(t: &OracleType) -> ValueKind {
    match t {
        OracleType::Number(p, s) => kind_by_name("NUMBER", (*p > 0).then_some(i64::from(*p)), (*s != -127).then_some(i64::from(*s))),
        OracleType::Int64 | OracleType::UInt64 => ValueKind::Int,
        OracleType::Float(_) => ValueKind::Decimal,
        OracleType::BinaryFloat | OracleType::BinaryDouble => ValueKind::Float,
        OracleType::Varchar2(_)
        | OracleType::NVarchar2(_)
        | OracleType::Char(_)
        | OracleType::NChar(_)
        | OracleType::CLOB
        | OracleType::NCLOB
        | OracleType::Long
        | OracleType::Rowid => ValueKind::Text,
        OracleType::Raw(_) | OracleType::LongRaw | OracleType::BLOB => ValueKind::Bytes,
        OracleType::Date | OracleType::Timestamp(_) => ValueKind::Timestamp,
        OracleType::TimestampTZ(_) | OracleType::TimestampLTZ(_) => ValueKind::TimestampTz,
        OracleType::IntervalDS(_, _) | OracleType::IntervalYM(_) => ValueKind::Interval,
        OracleType::Boolean => ValueKind::Bool,
        OracleType::Json => ValueKind::Json,
        _ => ValueKind::Other,
    }
}

/// Oracle prints `.5` and `-.5`; the neutral form has a leading zero.
fn normalize_number(s: String) -> String {
    if let Some(rest) = s.strip_prefix("-.") {
        format!("-0.{rest}")
    } else if s.starts_with('.') {
        format!("0{s}")
    } else {
        s
    }
}

/// Reads one cell.
pub fn read(v: &SqlValue) -> Value {
    if v.is_null().unwrap_or(false) {
        return Value::Null;
    }
    let t = match v.oracle_type() {
        Ok(t) => t.clone(),
        Err(e) => {
            return Value::Other {
                type_name: "?".into(),
                text: e.to_string(),
            }
        }
    };
    let other = |v: &SqlValue| Value::Other {
        type_name: t.to_string(),
        text: v.get::<String>().unwrap_or_else(|_| v.to_string()),
    };
    let r: Option<Value> = match &t {
        OracleType::Number(..) | OracleType::Float(_) | OracleType::Int64 | OracleType::UInt64 => v.get::<String>().ok().map(|s| {
            let s = normalize_number(s);
            match (kind_of(&t), s.parse::<i64>()) {
                (_, Ok(i)) if !s.contains('.') => Value::Int(i),
                _ => Value::Decimal(s),
            }
        }),
        OracleType::BinaryFloat | OracleType::BinaryDouble => v.get::<f64>().ok().map(Value::Float),
        OracleType::Varchar2(_)
        | OracleType::NVarchar2(_)
        | OracleType::Char(_)
        | OracleType::NChar(_)
        | OracleType::CLOB
        | OracleType::NCLOB
        | OracleType::Long
        | OracleType::Rowid => v.get::<String>().ok().map(Value::Text),
        OracleType::Raw(_) | OracleType::LongRaw | OracleType::BLOB => v.get::<Vec<u8>>().ok().map(Value::Bytes),
        OracleType::Date | OracleType::Timestamp(_) => v.get::<NaiveDateTime>().ok().map(Value::Timestamp),
        OracleType::TimestampTZ(_) | OracleType::TimestampLTZ(_) => v.get::<DateTime<FixedOffset>>().ok().map(Value::TimestampTz),
        OracleType::IntervalDS(..) => v.get::<IntervalDS>().ok().map(|i| Value::Interval(i.to_string())),
        OracleType::IntervalYM(_) => v.get::<IntervalYM>().ok().map(|i| Value::Interval(i.to_string())),
        OracleType::Boolean => v.get::<bool>().ok().map(Value::Bool),
        OracleType::Json => v.get::<String>().ok().map(Value::Json),
        _ => None,
    };
    r.unwrap_or_else(|| other(v))
}

/// A bind parameter for a value (NLS formats are set on connect, so decimal text converts implicitly).
pub fn to_bind(v: &Value) -> Box<dyn ToSql> {
    match v {
        Value::Null => Box::new(None::<String>),
        Value::Bool(b) => Box::new(if *b { "true" } else { "false" }.to_owned()),
        Value::Int(i) => Box::new(*i),
        Value::Float(f) => Box::new(*f),
        Value::Bytes(b) => Box::new(b.clone()),
        Value::Date(d) => Box::new(d.and_time(chrono::NaiveTime::MIN)),
        Value::Timestamp(t) => Box::new(*t),
        Value::TimestampTz(t) => Box::new(*t),
        Value::Uuid(u) => Box::new(u.replace('-', "").to_ascii_uppercase()),
        Value::Decimal(_)
        | Value::Text(_)
        | Value::Time(_)
        | Value::Interval(_)
        | Value::Json(_)
        | Value::Array(_)
        | Value::Other { .. } => Box::new(v.display().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_types_map_to_kinds() {
        assert_eq!(kind_by_name("NUMBER", Some(10), Some(0)), ValueKind::Int);
        assert_eq!(kind_by_name("NUMBER", Some(19), Some(0)), ValueKind::Decimal);
        assert_eq!(kind_by_name("NUMBER", None, None), ValueKind::Decimal);
        assert_eq!(kind_by_name("NUMBER", Some(12), Some(2)), ValueKind::Decimal);
        assert_eq!(kind_by_name("TIMESTAMP(6) WITH TIME ZONE", None, None), ValueKind::TimestampTz);
        assert_eq!(
            kind_by_name("TIMESTAMP(6) WITH LOCAL TIME ZONE", None, None),
            ValueKind::TimestampTz
        );
        assert_eq!(kind_by_name("DATE", None, None), ValueKind::Timestamp);
        assert_eq!(kind_by_name("INTERVAL DAY(2) TO SECOND(6)", None, None), ValueKind::Interval);
        assert_eq!(kind_by_name("NCLOB", None, None), ValueKind::Text);
        assert_eq!(kind_by_name("SDO_GEOMETRY", None, None), ValueKind::Other);
        assert_eq!(kind_of(&OracleType::Number(10, 0)), ValueKind::Int);
        assert_eq!(kind_of(&OracleType::Number(0, -127)), ValueKind::Decimal);
    }

    #[test]
    fn numbers_get_a_leading_zero() {
        assert_eq!(normalize_number(".5".into()), "0.5");
        assert_eq!(normalize_number("-.25".into()), "-0.25");
        assert_eq!(normalize_number("12".into()), "12");
    }
}
