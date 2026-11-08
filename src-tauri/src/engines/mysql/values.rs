//! MariaDB/MySQL values and column types as contract values and kinds, and back for bound parameters.
//!
//! The text protocol (`query_iter`, query tabs) sends every value as text and the binary protocol (`exec`, browse and
//! edits) sends typed values, so reading always goes by the column's kind: `Value::Bytes("12")` and `Value::Int(12)`
//! both become `Int(12)` in an integer column.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use mysql_async::{
    consts::{ColumnFlags, ColumnType},
    Column, Value as My,
};

use crate::{
    contract::{
        value::{format_float, parse_input, parse_time, parse_timestamp},
        ColumnHints, ColumnInfo, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

/// `character_set` of binary strings and blobs.
pub const BINARY_CHARSET: u16 = 63;
/// Result columns named like this, binary and a multiple of 4 bytes long, are read as vectors (MariaDB sends VECTOR as
/// VARBINARY in result sets).
const VECTOR_NAME_HINTS: [&str; 3] = ["embed", "vector", "vec"];

/// The neutral kind of a catalog column (`DATA_TYPE`, `COLUMN_TYPE`, whether it holds JSON).
pub fn kind_of(data_type: &str, column_type: &str, json: bool) -> ValueKind {
    let data_type = data_type.to_ascii_lowercase();
    let column_type = column_type.to_ascii_lowercase();
    if json {
        return ValueKind::Json;
    }
    match data_type.as_str() {
        "tinyint" if column_type.starts_with("tinyint(1)") => ValueKind::Bool,
        "bit" if column_type == "bit" || column_type == "bit(1)" => ValueKind::Bool,
        "bool" | "boolean" => ValueKind::Bool,
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => ValueKind::Int,
        "decimal" | "numeric" | "dec" | "fixed" => ValueKind::Decimal,
        "float" | "double" | "real" | "double precision" => ValueKind::Float,
        "date" => ValueKind::Date,
        "datetime" | "timestamp" => ValueKind::Timestamp,
        "time" => ValueKind::Time,
        "json" => ValueKind::Json,
        "uuid" => ValueKind::Uuid,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit" => ValueKind::Bytes,
        "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection"
        | "geomcollection" => ValueKind::Bytes,
        "vector" => ValueKind::Other,
        _ => ValueKind::Text,
    }
}

/// `vector(4)` → 4.
pub fn vector_dims(data_type: &str, column_type: &str) -> Option<u32> {
    if !data_type.eq_ignore_ascii_case("vector") {
        return None;
    }
    column_type.split_once('(')?.1.trim_end_matches(')').trim().parse().ok()
}

/// The SQL type a result column was declared with, from its wire type (`MYSQL_TYPE_LONG` is an `int`, `NEWDECIMAL` a
/// `decimal`).
pub fn sql_type_name(column_type: ColumnType, binary: bool, flags: ColumnFlags) -> &'static str {
    use ColumnType::*;
    if flags.contains(ColumnFlags::ENUM_FLAG) {
        return "enum";
    }
    if flags.contains(ColumnFlags::SET_FLAG) {
        return "set";
    }
    match column_type {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING if binary => "varbinary",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => "varchar",
        MYSQL_TYPE_STRING if binary => "binary",
        MYSQL_TYPE_STRING => "char",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB if binary => "blob",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB => "text",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_VECTOR => "vector",
        _ => "unknown",
    }
}

fn is_byte_string(column_type: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        column_type,
        MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
    )
}

/// Vector dimensions of a result column: a MySQL VECTOR, or a binary column named like a vector.
pub fn guess_vector_dims(column: &Column) -> Option<u32> {
    let length = column.column_length();
    if column.column_type() == ColumnType::MYSQL_TYPE_VECTOR {
        return (length > 0).then_some(length / 4);
    }
    let name = column.name_str().to_lowercase();
    let named_like_vector = VECTOR_NAME_HINTS.iter().any(|hint| name.contains(hint));
    let binary = column.character_set() == BINARY_CHARSET && is_byte_string(column.column_type());
    (named_like_vector && binary && length > 0 && length.is_multiple_of(4)).then_some(length / 4)
}

/// A result-set column (query tabs): kind and hints from the wire type, flags and character set.
pub fn result_column(column: &Column) -> ColumnInfo {
    use ColumnType::*;
    let column_type = column.column_type();
    let flags = column.flags();
    let binary = column.character_set() == BINARY_CHARSET;
    let vector = guess_vector_dims(column);
    let kind = if vector.is_some() {
        ValueKind::Other
    } else if flags.intersects(ColumnFlags::ENUM_FLAG | ColumnFlags::SET_FLAG) {
        ValueKind::Text
    } else {
        match column_type {
            MYSQL_TYPE_TINY if column.column_length() == 1 => ValueKind::Bool,
            MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => {
                ValueKind::Int
            }
            MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => ValueKind::Decimal,
            MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => ValueKind::Float,
            MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => ValueKind::Date,
            MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 | MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => ValueKind::Timestamp,
            MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => ValueKind::Time,
            MYSQL_TYPE_JSON => ValueKind::Json,
            MYSQL_TYPE_BIT if column.column_length() == 1 => ValueKind::Bool,
            MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY => ValueKind::Bytes,
            other if is_byte_string(other) && binary => ValueKind::Bytes,
            _ => ValueKind::Text,
        }
    };
    let type_name = if vector.is_some() {
        "vector"
    } else {
        sql_type_name(column_type, binary, flags)
    };
    let mut info = ColumnInfo::result(&column.name_str(), type_name, kind);
    info.nullable = !flags.contains(ColumnFlags::NOT_NULL_FLAG);
    info.hints = ColumnHints {
        primary: flags.contains(ColumnFlags::PRI_KEY_FLAG),
        binary: info.kind == ValueKind::Bytes,
        vector_dims: vector,
        read_only: false,
    };
    info
}

fn is_vector(column: &ColumnInfo) -> bool {
    column.hints.vector_dims.is_some() || column.type_name.to_ascii_lowercase().starts_with("vector")
}

/// Little-endian `f32`s (the VECTOR storage format).
pub fn decode_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect())
}

pub fn encode_vector(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

/// `[0.1,0.2]`: the form `VEC_FromText` reads and vector cells show.
pub fn vector_text(values: &[f32]) -> String {
    let joined = values.iter().map(f32::to_string).collect::<Vec<_>>().join(",");
    format!("[{joined}]")
}

/// `[0.1, 0.2]` or `0.1 0.2` as floats.
pub fn parse_vector(text: &str) -> AppResult<Vec<f32>> {
    let values = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>().map_err(|_| AppError::new(format!("'{part}' is not a number"))))
        .collect::<AppResult<Vec<_>>>()?;
    if values.is_empty() {
        return Err(AppError::new("The vector is empty"));
    }
    Ok(values)
}

fn text_of(bytes: Vec<u8>) -> Value {
    match String::from_utf8(bytes) {
        Ok(text) => Value::Text(text),
        Err(error) => Value::Bytes(error.into_bytes()),
    }
}

fn other(type_name: &str, text: String) -> Value {
    Value::Other {
        type_name: type_name.to_owned(),
        text,
    }
}

fn integer(text: &str) -> Option<Value> {
    let text = text.trim();
    text.parse::<i64>()
        .map(Value::Int)
        .ok()
        .or_else(|| text.parse::<u64>().ok().map(|u| Value::Decimal(u.to_string())))
}

fn date_time(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))?.and_hms_micro_opt(
        u32::from(hour),
        u32::from(minute),
        u32::from(second),
        micros,
    )
}

/// The server's text form of a DATE / DATETIME / TIMESTAMP (`YYYY-MM-DD[ HH:MM:SS[.ffffff]]`), read without a format
/// parser: query tabs read thousands of them. `None` for zero dates and anything else.
fn wire_datetime(bytes: &[u8]) -> Option<NaiveDateTime> {
    fn number(bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(bytes.iter().fold(0, |acc, b| acc * 10 + u32::from(b - b'0')))
    }
    let date = bytes.get(..10)?;
    if date[4] != b'-' || date[7] != b'-' {
        return None;
    }
    let day = NaiveDate::from_ymd_opt(number(&date[..4])? as i32, number(&date[5..7])?, number(&date[8..10])?)?;
    let rest = &bytes[10..];
    if rest.is_empty() {
        return day.and_hms_opt(0, 0, 0);
    }
    if rest.len() < 9 || rest[0] != b' ' || rest[3] != b':' || rest[6] != b':' {
        return None;
    }
    let (hour, minute, second) = (number(&rest[1..3])?, number(&rest[4..6])?, number(&rest[7..9])?);
    let micros = match &rest[9..] {
        [] => 0,
        [b'.', digits @ ..] if (1..=6).contains(&digits.len()) => number(digits)? * 10u32.pow(6 - digits.len() as u32),
        _ => return None,
    };
    day.and_hms_micro_opt(hour, minute, second, micros)
}

#[allow(clippy::too_many_arguments)]
fn zero_text(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32, with_time: bool) -> String {
    let date = format!("{year:04}-{month:02}-{day:02}");
    if !with_time {
        return date;
    }
    let fraction = if micros == 0 { String::new() } else { format!(".{micros:06}") };
    format!("{date} {hour:02}:{minute:02}:{second:02}{fraction}")
}

/// A value read from the server as the contract value of its column.
pub fn to_value(value: My, column: &ColumnInfo) -> Value {
    if matches!(value, My::NULL) {
        return Value::Null;
    }
    if is_vector(column) {
        return match value {
            My::Bytes(bytes) => match decode_vector(&bytes) {
                Some(values) => other("vector", vector_text(&values)),
                None => Value::Bytes(bytes),
            },
            other_value => text_value(other_value),
        };
    }
    match (&column.kind, value) {
        (ValueKind::Bool, My::Int(i)) => Value::Bool(i != 0),
        (ValueKind::Bool, My::UInt(u)) => Value::Bool(u != 0),
        (ValueKind::Bool, My::Bytes(bytes)) => match bytes.as_slice() {
            [0] | b"0" => Value::Bool(false),
            [1] | b"1" => Value::Bool(true),
            _ => match std::str::from_utf8(&bytes).ok().and_then(integer) {
                Some(Value::Int(i)) => Value::Bool(i != 0),
                _ => Value::Bytes(bytes),
            },
        },
        (ValueKind::Int, My::Int(i)) => Value::Int(i),
        (ValueKind::Int, My::UInt(u)) => i64::try_from(u).map(Value::Int).unwrap_or_else(|_| Value::Decimal(u.to_string())),
        (ValueKind::Int, My::Bytes(bytes)) => match std::str::from_utf8(&bytes).ok().and_then(integer) {
            Some(number) => number,
            None => text_of(bytes),
        },
        // Single precision: the shortest text that reads back as the same f32 (0.1, not 0.10000000149).
        (ValueKind::Float, My::Float(f)) => Value::Float(f.to_string().parse().unwrap_or(f64::from(f))),
        (ValueKind::Float, My::Double(d)) => Value::Float(d),
        (ValueKind::Float, My::Bytes(bytes)) => match std::str::from_utf8(&bytes).ok().and_then(|t| t.trim().parse::<f64>().ok()) {
            Some(number) => Value::Float(number),
            None => text_of(bytes),
        },
        (ValueKind::Decimal, My::Bytes(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => Value::Decimal(text),
            Err(error) => Value::Bytes(error.into_bytes()),
        },
        (ValueKind::Date | ValueKind::Timestamp, My::Date(year, month, day, hour, minute, second, micros)) => {
            let with_time = column.kind == ValueKind::Timestamp;
            match date_time(year, month, day, hour, minute, second, micros) {
                Some(stamp) if with_time => Value::Timestamp(stamp),
                Some(stamp) => Value::Date(stamp.date()),
                // Zero dates (0000-00-00) have no calendar value.
                None => other(
                    &column.type_name,
                    zero_text(year, month, day, hour, minute, second, micros, with_time),
                ),
            }
        }
        (ValueKind::Date, My::Bytes(bytes)) => match wire_datetime(&bytes) {
            Some(stamp) => Value::Date(stamp.date()),
            None => other(&column.type_name, String::from_utf8_lossy(&bytes).into_owned()),
        },
        (ValueKind::Timestamp, My::Bytes(bytes)) => match wire_datetime(&bytes) {
            Some(stamp) => Value::Timestamp(stamp),
            None => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                match parse_timestamp(text.trim()) {
                    Some(stamp) => Value::Timestamp(stamp),
                    None => other(&column.type_name, text),
                }
            }
        },
        (ValueKind::Time, My::Time(negative, days, hours, minutes, seconds, micros)) => {
            match (
                negative,
                days,
                NaiveTime::from_hms_micro_opt(u32::from(hours), u32::from(minutes), u32::from(seconds), micros),
            ) {
                (false, 0, Some(time)) => Value::Time(time),
                _ => {
                    let sign = if negative { "-" } else { "" };
                    let total_hours = days * 24 + u32::from(hours);
                    let fraction = if micros == 0 { String::new() } else { format!(".{micros:06}") };
                    Value::Interval(format!("{sign}{total_hours:02}:{minutes:02}:{seconds:02}{fraction}"))
                }
            }
        }
        (ValueKind::Time, My::Bytes(bytes)) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            match parse_time(text.trim()) {
                Some(time) => Value::Time(time),
                None => Value::Interval(text),
            }
        }
        (ValueKind::Json, My::Bytes(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => Value::Json(text),
            Err(error) => Value::Bytes(error.into_bytes()),
        },
        (ValueKind::Uuid, My::Bytes(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => Value::Uuid(text),
            Err(error) => Value::Bytes(error.into_bytes()),
        },
        (ValueKind::Bytes, My::Bytes(bytes)) => Value::Bytes(bytes),
        (_, value) => text_value(value),
    }
}

/// A value of an unexpected shape for its column, as text (bytes stay bytes when they are not UTF-8).
fn text_value(value: My) -> Value {
    match value {
        My::NULL => Value::Null,
        My::Bytes(bytes) => text_of(bytes),
        My::Int(i) => Value::Int(i),
        My::UInt(u) => i64::try_from(u).map(Value::Int).unwrap_or_else(|_| Value::Decimal(u.to_string())),
        My::Float(f) => Value::Float(f.to_string().parse().unwrap_or(f64::from(f))),
        My::Double(d) => Value::Float(d),
        My::Date(year, month, day, hour, minute, second, micros) => match date_time(year, month, day, hour, minute, second, micros) {
            Some(stamp) if (hour, minute, second, micros) == (0, 0, 0, 0) => Value::Date(stamp.date()),
            Some(stamp) => Value::Timestamp(stamp),
            None => Value::Text(zero_text(year, month, day, hour, minute, second, micros, true)),
        },
        My::Time(negative, days, hours, minutes, seconds, micros) => {
            let sign = if negative { "-" } else { "" };
            let fraction = if micros == 0 { String::new() } else { format!(".{micros:06}") };
            Value::Interval(format!(
                "{sign}{:02}:{minutes:02}:{seconds:02}{fraction}",
                days * 24 + u32::from(hours)
            ))
        }
    }
}

/// A value as text for catalog reads (`None` for NULL).
pub fn plain_text(value: My) -> Option<String> {
    match value {
        My::NULL => None,
        My::Bytes(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        My::Int(i) => Some(i.to_string()),
        My::UInt(u) => Some(u.to_string()),
        My::Float(f) => Some(f.to_string()),
        My::Double(d) => Some(format_float(d)),
        other_value => text_value(other_value).display(),
    }
}

/// What a user value means for `column`: text typed into a typed column is parsed (so a bad number is a clear error,
/// not a server warning), and text or a vector cell for a VECTOR column becomes its floats.
pub fn coerce(value: &Value, column: &ColumnInfo) -> AppResult<Value> {
    if is_vector(column) {
        return match value {
            Value::Text(text) | Value::Other { text, .. } => Ok(Value::Bytes(encode_vector(&parse_vector(text)?))),
            other_value => Ok(other_value.clone()),
        };
    }
    match (value, &column.kind) {
        (Value::Text(text), kind) if !matches!(kind, ValueKind::Text | ValueKind::Other | ValueKind::Json) => {
            parse_input(kind, text).map_err(AppError::from)
        }
        _ => Ok(value.clone()),
    }
}

/// A contract value as a bound parameter.
pub fn to_param(value: &Value) -> My {
    match value {
        Value::Null => My::NULL,
        Value::Bool(flag) => My::Int(i64::from(*flag)),
        Value::Int(i) => My::Int(*i),
        Value::Float(f) => My::Double(*f),
        Value::Decimal(text)
        | Value::Text(text)
        | Value::Interval(text)
        | Value::Uuid(text)
        | Value::Json(text)
        | Value::ObjectId(text) => My::Bytes(text.as_bytes().to_vec()),
        Value::Bytes(bytes) => My::Bytes(bytes.clone()),
        Value::Date(date) => My::Date(date.year_u16(), date.month_u8(), date.day_u8(), 0, 0, 0, 0),
        Value::Time(time) => My::Time(
            false,
            0,
            time.hour() as u8,
            time.minute() as u8,
            time.second() as u8,
            time.nanosecond() / 1000,
        ),
        Value::Timestamp(stamp) => stamp_param(stamp),
        Value::TimestampTz(stamp) => stamp_param(&stamp.naive_local()),
        Value::Array(_) | Value::Document(_) => My::Bytes(value.to_json().to_string().into_bytes()),
        Value::Other { type_name, text } if type_name.eq_ignore_ascii_case("vector") => match parse_vector(text) {
            Ok(values) => My::Bytes(encode_vector(&values)),
            Err(_) => My::Bytes(text.as_bytes().to_vec()),
        },
        Value::Other { text, .. } => My::Bytes(text.as_bytes().to_vec()),
    }
}

fn stamp_param(stamp: &NaiveDateTime) -> My {
    let date = stamp.date();
    let time = stamp.time();
    My::Date(
        date.year_u16(),
        date.month_u8(),
        date.day_u8(),
        time.hour() as u8,
        time.minute() as u8,
        time.second() as u8,
        time.nanosecond() / 1000,
    )
}

/// Narrow date parts for the wire.
trait DateParts {
    fn year_u16(&self) -> u16;
    fn month_u8(&self) -> u8;
    fn day_u8(&self) -> u8;
}

impl DateParts for NaiveDate {
    fn year_u16(&self) -> u16 {
        use chrono::Datelike;
        u16::try_from(self.year()).unwrap_or(0)
    }
    fn month_u8(&self) -> u8 {
        use chrono::Datelike;
        self.month() as u8
    }
    fn day_u8(&self) -> u8 {
        use chrono::Datelike;
        self.day() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(type_name: &str, kind: ValueKind) -> ColumnInfo {
        ColumnInfo::result("c", type_name, kind)
    }

    #[test]
    fn catalog_types_map_to_neutral_kinds() {
        assert_eq!(kind_of("tinyint", "tinyint(1)", false), ValueKind::Bool);
        assert_eq!(kind_of("tinyint", "tinyint(4)", false), ValueKind::Int);
        assert_eq!(kind_of("bigint", "bigint(20) unsigned", false), ValueKind::Int);
        assert_eq!(kind_of("decimal", "decimal(12,2)", false), ValueKind::Decimal);
        assert_eq!(kind_of("datetime", "datetime(3)", false), ValueKind::Timestamp);
        assert_eq!(kind_of("varbinary", "varbinary(64)", false), ValueKind::Bytes);
        assert_eq!(kind_of("bit", "bit(8)", false), ValueKind::Bytes);
        assert_eq!(kind_of("longtext", "longtext", true), ValueKind::Json);
        assert_eq!(kind_of("enum", "enum('a','b')", false), ValueKind::Text);
        assert_eq!(kind_of("vector", "vector(4)", false), ValueKind::Other);
        assert_eq!(vector_dims("vector", "vector(1536)"), Some(1536));
        assert_eq!(vector_dims("varbinary", "varbinary(16)"), None);
    }

    #[test]
    fn text_and_binary_protocol_values_read_the_same() {
        let int = column("int(11)", ValueKind::Int);
        assert_eq!(to_value(My::Bytes(b"42".to_vec()), &int), Value::Int(42));
        assert_eq!(to_value(My::Int(42), &int), Value::Int(42));
        assert_eq!(to_value(My::UInt(u64::MAX), &int), Value::Decimal(u64::MAX.to_string()));
        assert_eq!(
            to_value(My::Bytes(b"18446744073709551615".to_vec()), &int),
            Value::Decimal("18446744073709551615".into())
        );
        let decimal = column("decimal(12,2)", ValueKind::Decimal);
        assert_eq!(to_value(My::Bytes(b"12.50".to_vec()), &decimal), Value::Decimal("12.50".into()));
        let flag = column("tinyint(1)", ValueKind::Bool);
        assert_eq!(to_value(My::Int(1), &flag), Value::Bool(true));
        assert_eq!(to_value(My::Bytes(b"0".to_vec()), &flag), Value::Bool(false));
        assert_eq!(to_value(My::Bytes(vec![1]), &column("bit(1)", ValueKind::Bool)), Value::Bool(true));
        let float = column("float", ValueKind::Float);
        assert_eq!(to_value(My::Float(0.1), &float), Value::Float(0.1));
        assert_eq!(to_value(My::NULL, &float), Value::Null);
    }

    #[test]
    fn temporal_values_keep_their_type_and_zero_dates_stay_readable() {
        let stamp = column("datetime(3)", ValueKind::Timestamp);
        let value = to_value(My::Date(2026, 1, 1, 8, 0, 0, 250_000), &stamp);
        assert_eq!(value.display().unwrap(), "2026-01-01 08:00:00.250");
        assert_eq!(
            to_value(My::Bytes(b"2026-01-01 08:00:00.250".to_vec()), &stamp),
            value,
            "text protocol"
        );
        let date = column("date", ValueKind::Date);
        assert_eq!(to_value(My::Date(2020, 1, 2, 0, 0, 0, 0), &date).display().unwrap(), "2020-01-02");
        assert_eq!(to_value(My::Bytes(b"2020-01-02".to_vec()), &date).display().unwrap(), "2020-01-02");
        assert_eq!(
            to_value(My::Bytes(b"2026-01-01 08:00:00.5".to_vec()), &stamp).display().unwrap(),
            "2026-01-01 08:00:00.500"
        );
        assert_eq!(wire_datetime(b"2026-01-01 08:00:00"), parse_timestamp("2026-01-01 08:00:00"));
        assert_eq!(wire_datetime(b"2026-13-01"), None);
        assert_eq!(wire_datetime(b"2026-01-01T08:00:00"), None);
        let zero = to_value(My::Bytes(b"0000-00-00".to_vec()), &date);
        assert!(matches!(&zero, Value::Other { text, .. } if text == "0000-00-00"), "{zero:?}");
        let time = column("time", ValueKind::Time);
        assert_eq!(to_value(My::Time(false, 0, 8, 30, 0, 0), &time).display().unwrap(), "08:30:00");
        assert_eq!(to_value(My::Time(true, 1, 2, 0, 0, 0), &time), Value::Interval("-26:00:00".into()));
    }

    #[test]
    fn binary_json_and_vectors_map_to_their_variants() {
        let blob = column("varbinary(4)", ValueKind::Bytes);
        assert_eq!(to_value(My::Bytes(vec![0, 1]), &blob), Value::Bytes(vec![0, 1]));
        let json = column("json", ValueKind::Json);
        assert_eq!(to_value(My::Bytes(br#"{"n":1}"#.to_vec()), &json), Value::Json(r#"{"n":1}"#.into()));
        let mut vector = column("vector(2)", ValueKind::Other);
        vector.hints.vector_dims = Some(2);
        let bytes = encode_vector(&[0.5, -1.0]);
        let value = to_value(My::Bytes(bytes.clone()), &vector);
        assert_eq!(
            value,
            Value::Other {
                type_name: "vector".into(),
                text: "[0.5,-1]".into()
            }
        );
        assert_eq!(to_param(&value), My::Bytes(bytes.clone()));
        assert_eq!(coerce(&Value::Text("[0.5, -1]".into()), &vector).unwrap(), Value::Bytes(bytes));
    }

    #[test]
    fn parameters_are_coerced_to_the_column_kind() {
        let int = column("int", ValueKind::Int);
        assert_eq!(coerce(&Value::Text("7".into()), &int).unwrap(), Value::Int(7));
        assert!(coerce(&Value::Text("seven".into()), &int).is_err());
        let blob = column("varbinary(4)", ValueKind::Bytes);
        assert_eq!(
            coerce(&Value::Text("0xdead".into()), &blob).unwrap(),
            Value::Bytes(vec![0xde, 0xad])
        );
        let text = column("varchar(5)", ValueKind::Text);
        assert_eq!(coerce(&Value::Text("x".into()), &text).unwrap(), Value::Text("x".into()));
        assert_eq!(to_param(&Value::Bool(true)), My::Int(1));
        assert_eq!(to_param(&Value::Decimal("9.99".into())), My::Bytes(b"9.99".to_vec()));
        let stamp = parse_timestamp("2026-01-01 08:00:00.25").unwrap();
        assert_eq!(to_param(&Value::Timestamp(stamp)), My::Date(2026, 1, 1, 8, 0, 0, 250_000));
        let date = NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
        assert_eq!(to_param(&Value::Date(date)), My::Date(2020, 1, 2, 0, 0, 0, 0));
    }
}
