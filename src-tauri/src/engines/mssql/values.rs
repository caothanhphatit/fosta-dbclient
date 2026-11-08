//! SQL Server values and column types as contract values and kinds, and back for bound parameters.
//!
//! Reading goes by the TDS value (`ColumnData`), so table pages and query tabs read the same; parameters are typed after
//! the column they are compared with (an `int` key is bound as `int`, a `real` as `real`), so the server can seek on
//! indexes and exact comparisons hold.

use std::borrow::Cow;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use tiberius::{numeric::Numeric, Column, ColumnData, ColumnType, FromSql, IntoSql, ToSql, Uuid};

use crate::{
    contract::{
        value::{format_float, hex, parse_input},
        ColumnHints, ColumnInfo, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

/// The base type of a column (`nvarchar(100)` → `nvarchar`).
pub fn base_type(column: &ColumnInfo) -> String {
    let lower = column.type_name.to_ascii_lowercase();
    lower.split('(').next().unwrap_or_default().trim().to_owned()
}

/// The neutral kind of a catalog type (the system type name).
pub fn kind_of(base: &str) -> ValueKind {
    match base.to_ascii_lowercase().as_str() {
        "bit" => ValueKind::Bool,
        "tinyint" | "smallint" | "int" | "bigint" => ValueKind::Int,
        "decimal" | "numeric" | "money" | "smallmoney" => ValueKind::Decimal,
        "float" | "real" => ValueKind::Float,
        "date" => ValueKind::Date,
        "time" => ValueKind::Time,
        "datetime" | "datetime2" | "smalldatetime" => ValueKind::Timestamp,
        "datetimeoffset" => ValueKind::TimestampTz,
        "uniqueidentifier" => ValueKind::Uuid,
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => ValueKind::Bytes,
        "xml" | "sql_variant" | "hierarchyid" | "geometry" | "geography" => ValueKind::Other,
        _ => ValueKind::Text,
    }
}

/// Columns of these CLR types are read as text (`col.ToString()`): tiberius cannot read UDT values.
pub fn is_clr_type(base: &str) -> bool {
    matches!(base, "hierarchyid" | "geometry" | "geography")
}

/// The declared type as the design view shows it: `nvarchar(100)`, `varbinary(max)`, `decimal(12,2)`, `datetime2(3)`.
/// `max_length` is in bytes as `sys.columns` reports it (-1 for max); `timestamp` reads as `rowversion`.
pub fn type_label(base: &str, max_length: i64, precision: i64, scale: i64) -> String {
    let base = base.to_ascii_lowercase();
    match base.as_str() {
        "timestamp" => "rowversion".to_owned(),
        "varchar" | "char" | "varbinary" | "binary" => match max_length {
            -1 => format!("{base}(max)"),
            n => format!("{base}({n})"),
        },
        "nvarchar" | "nchar" => match max_length {
            -1 => format!("{base}(max)"),
            n => format!("{base}({})", n / 2),
        },
        "decimal" | "numeric" => format!("{base}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" if scale != 7 => format!("{base}({scale})"),
        _ => base,
    }
}

/// The SQL Server type a result column was declared with, from its TDS type (`Int4` is an `int`, `BigVarChar` a
/// `varchar`).
pub fn sql_type_name(column_type: ColumnType) -> &'static str {
    use ColumnType::*;
    match column_type {
        Null => "null",
        Bit | Bitn => "bit",
        Int1 => "tinyint",
        Int2 => "smallint",
        Int4 | Intn => "int",
        Int8 => "bigint",
        Float4 => "real",
        Float8 | Floatn => "float",
        Money => "money",
        Money4 => "smallmoney",
        Decimaln => "decimal",
        Numericn => "numeric",
        Guid => "uniqueidentifier",
        Datetime4 => "smalldatetime",
        Datetime | Datetimen => "datetime",
        Daten => "date",
        Timen => "time",
        Datetime2 => "datetime2",
        DatetimeOffsetn => "datetimeoffset",
        BigVarBin => "varbinary",
        BigBinary => "binary",
        BigVarChar => "varchar",
        BigChar => "char",
        NVarchar => "nvarchar",
        NChar => "nchar",
        Xml => "xml",
        Udt => "udt",
        Text => "text",
        NText => "ntext",
        Image => "image",
        SSVariant => "sql_variant",
    }
}

/// A result-set column (query tabs): the kind from its TDS type.
pub fn result_column(column: &Column) -> ColumnInfo {
    let type_name = sql_type_name(column.column_type());
    let kind = match type_name {
        "udt" | "null" => ValueKind::Other,
        other => kind_of(other),
    };
    let mut info = ColumnInfo::result(column.name(), type_name, kind);
    info.hints = ColumnHints {
        binary: info.kind == ValueKind::Bytes,
        ..ColumnHints::default()
    };
    info
}

/// `12.50` from the unscaled value and the scale (tiberius' own Display mangles negative values).
pub fn numeric_text(number: Numeric) -> String {
    let scale = number.scale() as usize;
    let value = number.value();
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// Money arrives as a float with four decimals.
fn money_text(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A value read from the server as the contract value of its column.
pub fn to_value(data: ColumnData<'static>, column: &ColumnInfo) -> Value {
    match data {
        ColumnData::U8(value) => value.map_or(Value::Null, |n| Value::Int(i64::from(n))),
        ColumnData::I16(value) => value.map_or(Value::Null, |n| Value::Int(i64::from(n))),
        ColumnData::I32(value) => value.map_or(Value::Null, |n| Value::Int(i64::from(n))),
        ColumnData::I64(value) => value.map_or(Value::Null, Value::Int),
        // Single precision: the shortest text that reads back as the same f32 (0.1, not 0.10000000149).
        ColumnData::F32(value) => value.map_or(Value::Null, |f| Value::Float(f.to_string().parse().unwrap_or(f64::from(f)))),
        ColumnData::F64(value) => match value {
            None => Value::Null,
            Some(f) if column.kind == ValueKind::Decimal => Value::Decimal(money_text(f)),
            Some(f) => Value::Float(f),
        },
        ColumnData::Bit(value) => value.map_or(Value::Null, Value::Bool),
        ColumnData::String(value) => value.map_or(Value::Null, |text| match column.kind {
            ValueKind::Json => Value::Json(text.into_owned()),
            ValueKind::Other => Value::Other {
                type_name: base_type(column),
                text: text.into_owned(),
            },
            _ => Value::Text(text.into_owned()),
        }),
        ColumnData::Guid(value) => value.map_or(Value::Null, |guid| Value::Uuid(guid.to_string())),
        ColumnData::Binary(value) => value.map_or(Value::Null, |bytes| Value::Bytes(bytes.into_owned())),
        ColumnData::Numeric(value) => value.map_or(Value::Null, |number| Value::Decimal(numeric_text(number))),
        ColumnData::Xml(value) => value.map_or(Value::Null, |xml| Value::Other {
            type_name: "xml".into(),
            text: xml.into_owned().into_string(),
        }),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            temporal(NaiveDateTime::from_sql(&data), Value::Timestamp)
        }
        ColumnData::Date(_) => temporal(NaiveDate::from_sql(&data), Value::Date),
        ColumnData::Time(_) => temporal(NaiveTime::from_sql(&data), Value::Time),
        ColumnData::DateTimeOffset(_) => temporal(DateTime::<FixedOffset>::from_sql(&data), Value::TimestampTz),
    }
}

fn temporal<T>(read: tiberius::Result<Option<T>>, wrap: fn(T) -> Value) -> Value {
    match read {
        Ok(Some(value)) => wrap(value),
        _ => Value::Null,
    }
}

/// A value as text for catalog reads (`None` for NULL).
pub fn plain_text(data: ColumnData<'static>) -> Option<String> {
    match data {
        ColumnData::String(value) => value.map(Cow::into_owned),
        ColumnData::Binary(value) => value.map(|bytes| hex(&bytes)),
        other => to_value(other, &ColumnInfo::result("", "", ValueKind::Text)).display(),
    }
}

/// What a user value means for `column`: text typed into a typed column is parsed (so a bad number is a clear error, not
/// a conversion failure on the server).
pub fn coerce(value: &Value, column: &ColumnInfo) -> AppResult<Value> {
    match (value, &column.kind) {
        (Value::Text(text), kind) if !matches!(kind, ValueKind::Text | ValueKind::Other | ValueKind::Json) => {
            parse_input(kind, text).map_err(AppError::from)
        }
        _ => Ok(value.clone()),
    }
}

/// A bound parameter: an owned TDS value.
#[derive(Clone, Debug, PartialEq)]
pub struct Param(pub ColumnData<'static>);

impl ToSql for Param {
    fn to_sql(&self) -> ColumnData<'_> {
        self.0.clone()
    }
}

/// `12.50` as a TDS numeric (scale at most 37).
pub fn parse_numeric(text: &str) -> Option<Numeric> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    if fraction.len() > 37 {
        return None;
    }
    let unscaled: i128 = format!("{whole}{fraction}").parse().ok()?;
    Some(Numeric::new_with_scale(
        if negative { -unscaled } else { unscaled },
        fraction.len() as u8,
    ))
}

/// A typed NULL for a column, so the server never has to convert an `nvarchar` NULL (not allowed for binary columns).
fn null_for(column: Option<&ColumnInfo>) -> ColumnData<'static> {
    let Some(column) = column else {
        return ColumnData::String(None);
    };
    match column.kind {
        ValueKind::Bool => ColumnData::Bit(None),
        ValueKind::Int => ColumnData::I64(None),
        ValueKind::Float => ColumnData::F64(None),
        ValueKind::Decimal => ColumnData::Numeric(None),
        ValueKind::Bytes => ColumnData::Binary(None),
        ValueKind::Uuid => ColumnData::Guid(None),
        ValueKind::Date => ColumnData::Date(None),
        ValueKind::Time => ColumnData::Time(None),
        ValueKind::Timestamp => ColumnData::DateTime2(None),
        ValueKind::TimestampTz => ColumnData::DateTimeOffset(None),
        _ => ColumnData::String(None),
    }
}

fn int_for(base: &str, i: i64) -> ColumnData<'static> {
    match base {
        "tinyint" => u8::try_from(i).map_or(ColumnData::I64(Some(i)), |n| ColumnData::U8(Some(n))),
        "smallint" => i16::try_from(i).map_or(ColumnData::I64(Some(i)), |n| ColumnData::I16(Some(n))),
        "int" => i32::try_from(i).map_or(ColumnData::I64(Some(i)), |n| ColumnData::I32(Some(n))),
        "bit" => ColumnData::Bit(Some(i != 0)),
        "decimal" | "numeric" | "money" | "smallmoney" => ColumnData::Numeric(Some(Numeric::new_with_scale(i128::from(i), 0))),
        "float" => ColumnData::F64(Some(i as f64)),
        "real" => ColumnData::F32(Some(i as f32)),
        _ => ColumnData::I64(Some(i)),
    }
}

fn text(text: &str) -> ColumnData<'static> {
    ColumnData::String(Some(Cow::Owned(text.to_owned())))
}

/// A contract value as a parameter typed for `column` (or by the value alone when it is compared with no column).
pub fn to_param(value: &Value, column: Option<&ColumnInfo>) -> Param {
    let base = column.map(base_type).unwrap_or_default();
    let data = match value {
        Value::Null => null_for(column),
        Value::Bool(flag) => ColumnData::Bit(Some(*flag)),
        Value::Int(i) => int_for(&base, *i),
        Value::Float(f) => match base.as_str() {
            "real" => ColumnData::F32(Some(*f as f32)),
            "decimal" | "numeric" | "money" | "smallmoney" => match parse_numeric(&format_float(*f)) {
                Some(number) => ColumnData::Numeric(Some(number)),
                None => ColumnData::F64(Some(*f)),
            },
            _ => ColumnData::F64(Some(*f)),
        },
        Value::Decimal(number) => match base.as_str() {
            "float" => number.parse().map_or_else(|_| text(number), |f| ColumnData::F64(Some(f))),
            "real" => number.parse().map_or_else(|_| text(number), |f| ColumnData::F32(Some(f))),
            "tinyint" | "smallint" | "int" | "bigint" => number.parse().map_or_else(|_| text(number), |i| int_for(&base, i)),
            _ => parse_numeric(number).map_or_else(|| text(number), |n| ColumnData::Numeric(Some(n))),
        },
        Value::Text(value) | Value::Interval(value) | Value::Json(value) | Value::ObjectId(value) => text(value),
        Value::Uuid(value) => Uuid::parse_str(value).map_or_else(|_| text(value), |guid| ColumnData::Guid(Some(guid))),
        Value::Bytes(bytes) => ColumnData::Binary(Some(Cow::Owned(bytes.clone()))),
        Value::Date(date) => (*date).into_sql(),
        Value::Time(time) => (*time).into_sql(),
        Value::Timestamp(stamp) => (*stamp).into_sql(),
        Value::TimestampTz(stamp) => (*stamp).into_sql(),
        Value::Array(_) | Value::Document(_) => text(&value.to_json().to_string()),
        // XML too: written to an xml column it converts implicitly, and row checks compare xml as text.
        Value::Other { text: value, .. } => text(value),
    };
    Param(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiberius::xml::XmlData;

    fn column(type_name: &str) -> ColumnInfo {
        ColumnInfo::result("c", type_name, kind_of(type_name.split('(').next().unwrap()))
    }

    #[test]
    fn catalog_types_map_to_neutral_kinds_and_labels() {
        assert_eq!(kind_of("bit"), ValueKind::Bool);
        assert_eq!(kind_of("money"), ValueKind::Decimal);
        assert_eq!(kind_of("datetime2"), ValueKind::Timestamp);
        assert_eq!(kind_of("datetimeoffset"), ValueKind::TimestampTz);
        assert_eq!(kind_of("uniqueidentifier"), ValueKind::Uuid);
        assert_eq!(kind_of("timestamp"), ValueKind::Bytes, "rowversion");
        assert_eq!(kind_of("xml"), ValueKind::Other);
        assert_eq!(kind_of("nvarchar"), ValueKind::Text);
        assert_eq!(type_label("nvarchar", 200, 0, 0), "nvarchar(100)");
        assert_eq!(type_label("nvarchar", -1, 0, 0), "nvarchar(max)");
        assert_eq!(type_label("varbinary", 64, 0, 0), "varbinary(64)");
        assert_eq!(type_label("decimal", 9, 12, 2), "decimal(12,2)");
        assert_eq!(type_label("datetime2", 7, 23, 3), "datetime2(3)");
        assert_eq!(type_label("datetime2", 8, 27, 7), "datetime2");
        assert_eq!(type_label("timestamp", 8, 0, 0), "rowversion");
        assert_eq!(type_label("int", 4, 10, 0), "int");
    }

    #[test]
    fn tds_values_become_typed_values() {
        let int = column("int");
        assert_eq!(to_value(ColumnData::I32(Some(7)), &int), Value::Int(7));
        assert_eq!(to_value(ColumnData::U8(Some(255)), &int), Value::Int(255));
        assert_eq!(to_value(ColumnData::I64(None), &int), Value::Null);
        assert_eq!(to_value(ColumnData::Bit(Some(true)), &column("bit")), Value::Bool(true));
        let decimal = column("decimal(12,2)");
        assert_eq!(
            to_value(ColumnData::Numeric(Some(Numeric::new_with_scale(1250, 2))), &decimal),
            Value::Decimal("12.50".into())
        );
        assert_eq!(
            to_value(ColumnData::Numeric(Some(Numeric::new_with_scale(-5, 2))), &decimal),
            Value::Decimal("-0.05".into())
        );
        assert_eq!(numeric_text(Numeric::new_with_scale(42, 0)), "42");
        assert_eq!(
            to_value(ColumnData::F64(Some(12.5)), &column("money")),
            Value::Decimal("12.5".into())
        );
        assert_eq!(to_value(ColumnData::F32(Some(0.1)), &column("real")), Value::Float(0.1));
        assert_eq!(to_value(ColumnData::F64(Some(0.25)), &column("float")), Value::Float(0.25));
        assert_eq!(
            to_value(ColumnData::String(Some("x".into())), &column("nvarchar(5)")),
            Value::Text("x".into())
        );
        assert_eq!(
            to_value(ColumnData::Binary(Some(vec![0, 1].into())), &column("rowversion")),
            Value::Bytes(vec![0, 1])
        );
        let guid = Uuid::parse_str("6f9619ff-8b86-d011-b42d-00c04fc964ff").unwrap();
        assert_eq!(
            to_value(ColumnData::Guid(Some(guid)), &column("uniqueidentifier")),
            Value::Uuid("6f9619ff-8b86-d011-b42d-00c04fc964ff".into())
        );
        let xml = to_value(ColumnData::Xml(Some(Cow::Owned(XmlData::new("<a/>")))), &column("xml"));
        assert_eq!(
            xml,
            Value::Other {
                type_name: "xml".into(),
                text: "<a/>".into()
            }
        );
    }

    #[test]
    fn temporal_values_keep_their_type() {
        let stamp = NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_milli_opt(8, 6, 0, 250)
            .unwrap();
        let read = to_value(stamp.into_sql(), &column("datetime2(3)"));
        assert_eq!(read, Value::Timestamp(stamp));
        assert_eq!(read.display().as_deref(), Some("2026-01-01 08:06:00.250"));
        let date = NaiveDate::from_ymd_opt(2020, 1, 7).unwrap();
        assert_eq!(to_value(date.into_sql(), &column("date")), Value::Date(date));
        let time = NaiveTime::from_hms_opt(8, 30, 0).unwrap();
        assert_eq!(to_value(time.into_sql(), &column("time")), Value::Time(time));
        let offset = DateTime::parse_from_rfc3339("2026-01-01T08:00:00+07:00").unwrap();
        assert_eq!(
            to_value(offset.into_sql(), &column("datetimeoffset")).display().as_deref(),
            Some("2026-01-01 08:00:00+07:00")
        );
    }

    #[test]
    fn parameters_are_typed_after_their_column() {
        assert_eq!(to_param(&Value::Int(5), Some(&column("int"))).0, ColumnData::I32(Some(5)));
        assert_eq!(to_param(&Value::Int(5), Some(&column("tinyint"))).0, ColumnData::U8(Some(5)));
        assert_eq!(to_param(&Value::Int(5), Some(&column("bigint"))).0, ColumnData::I64(Some(5)));
        assert_eq!(to_param(&Value::Int(1), Some(&column("bit"))).0, ColumnData::Bit(Some(true)));
        assert_eq!(to_param(&Value::Float(0.1), Some(&column("real"))).0, ColumnData::F32(Some(0.1)));
        let price = to_param(&Value::Decimal("9.99".into()), Some(&column("decimal(12,2)"))).0;
        assert_eq!(price, ColumnData::Numeric(Some(Numeric::new_with_scale(999, 2))));
        assert_eq!(parse_numeric("-0.5").map(numeric_text).as_deref(), Some("-0.5"));
        assert!(parse_numeric("1e5").is_none());
        assert_eq!(to_param(&Value::Null, Some(&column("varbinary(64)"))).0, ColumnData::Binary(None));
        assert_eq!(to_param(&Value::Null, None).0, ColumnData::String(None));
        assert_eq!(
            to_param(&Value::Bytes(vec![1, 2]), None).0,
            ColumnData::Binary(Some(Cow::Owned(vec![1, 2])))
        );
        assert!(matches!(
            to_param(&Value::Uuid("6f9619ff-8b86-d011-b42d-00c04fc964ff".into()), None).0,
            ColumnData::Guid(Some(_))
        ));
        let int = column("int");
        assert_eq!(coerce(&Value::Text("7".into()), &int).unwrap(), Value::Int(7));
        assert!(coerce(&Value::Text("seven".into()), &int).is_err());
        assert_eq!(
            coerce(&Value::Text("0xcafe".into()), &column("varbinary(4)")).unwrap(),
            Value::Bytes(vec![0xca, 0xfe])
        );
    }
}
