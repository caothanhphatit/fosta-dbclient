//! PostgreSQL type mapping. tokio-postgres always asks for binary results, so cells are read raw (any type is accepted) and
//! decoded here by type: built-ins, arrays, ranges, composites, enums and domains. Parameters go the other way as text with a
//! server-side cast (`($1::text)::numeric(12,2)`), so every type the server can parse from text can be filtered and edited.

use crate::engines::common::value::{format_float, uuid_from_bytes};
use crate::engines::common::{Value, ValueKind};
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use std::fmt::Write as _;
use tokio_postgres::types::{FromSql, Kind, Type};

/// A raw cell: the type and the binary bytes, or NULL.
pub struct Raw(pub Option<Vec<u8>>);

impl<'a> FromSql<'a> for Raw {
    fn from_sql(_ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(Some(raw.to_vec())))
    }
    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(None))
    }
    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Reads every cell of a row.
pub fn row_values(row: &tokio_postgres::Row) -> Vec<Value> {
    (0..row.len())
        .map(|i| {
            let ty = row.columns()[i].type_();
            match row.try_get::<_, Raw>(i) {
                Ok(Raw(Some(bytes))) => decode(ty, &bytes),
                Ok(Raw(None)) => Value::Null,
                Err(e) => Value::Other {
                    type_name: ty.name().to_owned(),
                    text: format!("<{e}>"),
                },
            }
        })
        .collect()
}

/// The neutral kind of a result column.
pub fn kind_of(ty: &Type) -> ValueKind {
    match ty.kind() {
        Kind::Array(elem) => ValueKind::Array(Box::new(kind_of(elem))),
        Kind::Domain(base) => kind_of(base),
        Kind::Enum(_) => ValueKind::Text,
        Kind::Simple => kind_by_name(ty.name(), ""),
        _ => ValueKind::Other,
    }
}

/// The neutral kind of a type by its (base) name, with pg_type.typcategory as the fallback.
pub fn kind_by_name(name: &str, category: &str) -> ValueKind {
    match name {
        "bool" => ValueKind::Bool,
        "int2" | "int4" | "int8" | "oid" | "xid" | "cid" | "regclass" | "regproc" | "regtype" => ValueKind::Int,
        "float4" | "float8" => ValueKind::Float,
        "numeric" | "money" => ValueKind::Decimal,
        "text" | "varchar" | "bpchar" | "name" | "char" | "citext" | "unknown" => ValueKind::Text,
        "bytea" => ValueKind::Bytes,
        "date" => ValueKind::Date,
        "time" => ValueKind::Time,
        "timestamp" => ValueKind::Timestamp,
        "timestamptz" => ValueKind::TimestampTz,
        "interval" => ValueKind::Interval,
        "uuid" => ValueKind::Uuid,
        "json" | "jsonb" => ValueKind::Json,
        _ => match category {
            "E" => ValueKind::Text,
            "S" => ValueKind::Text,
            _ => ValueKind::Other,
        },
    }
}

fn be_i16(b: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be_i64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn be_f64(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn pg_epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2000, 1, 1).expect("valid date").and_time(NaiveTime::MIN)
}

/// Decodes one binary cell. Anything unexpected becomes `Other` with the text (UTF-8) or hex of the bytes, never a panic.
pub fn decode(ty: &Type, b: &[u8]) -> Value {
    let other = |text: String| Value::Other {
        type_name: ty.name().to_owned(),
        text,
    };
    match ty.kind() {
        Kind::Array(elem) => return decode_array(elem, b).unwrap_or_else(|| fallback(ty, b)),
        Kind::Domain(base) => return decode(base, b),
        Kind::Enum(_) => return Value::Text(String::from_utf8_lossy(b).into_owned()),
        Kind::Range(sub) => return decode_range(sub, b).map(other).unwrap_or_else(|| fallback(ty, b)),
        Kind::Composite(fields) => return decode_composite(fields, b).map(other).unwrap_or_else(|| fallback(ty, b)),
        _ => {}
    }
    let v = match *ty {
        Type::BOOL => b.first().map(|x| Value::Bool(*x != 0)),
        Type::INT2 => be_i16(b, 0).map(|v| Value::Int(v.into())),
        Type::INT4 => be_i32(b, 0).map(|v| Value::Int(v.into())),
        Type::INT8 => be_i64(b, 0).map(Value::Int),
        Type::OID | Type::XID | Type::CID | Type::REGCLASS | Type::REGPROC | Type::REGTYPE => {
            be_i32(b, 0).map(|v| Value::Int(i64::from(v as u32)))
        }
        Type::FLOAT4 => b
            .get(0..4)
            .and_then(|s| s.try_into().ok())
            .map(|a| Value::Float(f64::from(f32::from_be_bytes(a)))),
        Type::FLOAT8 => be_f64(b, 0).map(Value::Float),
        Type::NUMERIC => decode_numeric(b).map(|s| {
            if s == "NaN" || s.ends_with("Infinity") {
                other(s)
            } else {
                Value::Decimal(s)
            }
        }),
        Type::MONEY => be_i64(b, 0).map(|cents| Value::Decimal(scaled(cents as i128, 2))),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN | Type::CHAR => {
            Some(Value::Text(String::from_utf8_lossy(b).into_owned()))
        }
        Type::BYTEA => Some(Value::Bytes(b.to_vec())),
        Type::DATE => be_i32(b, 0).map(|d| match d {
            i32::MAX => other("infinity".into()),
            i32::MIN => other("-infinity".into()),
            d => pg_epoch()
                .date()
                .checked_add_signed(Duration::days(d.into()))
                .map(Value::Date)
                .unwrap_or_else(|| other(d.to_string())),
        }),
        Type::TIME => be_i64(b, 0).and_then(time_of_micros).map(Value::Time),
        Type::TIMETZ => {
            let t = be_i64(b, 0).and_then(time_of_micros);
            let off = be_i32(b, 8);
            t.zip(off)
                .map(|(t, off)| other(format!("{}{}", t.format("%H:%M:%S%.f"), offset_text(-off))))
        }
        Type::TIMESTAMP => be_i64(b, 0).map(|us| match us {
            i64::MAX => other("infinity".into()),
            i64::MIN => other("-infinity".into()),
            us => pg_epoch()
                .checked_add_signed(Duration::microseconds(us))
                .map(Value::Timestamp)
                .unwrap_or_else(|| other(us.to_string())),
        }),
        Type::TIMESTAMPTZ => be_i64(b, 0).map(|us| match us {
            i64::MAX => other("infinity".into()),
            i64::MIN => other("-infinity".into()),
            us => pg_epoch()
                .checked_add_signed(Duration::microseconds(us))
                .map(|n| Value::TimestampTz(Utc.from_utc_datetime(&n).fixed_offset()))
                .unwrap_or_else(|| other(us.to_string())),
        }),
        Type::INTERVAL => be_i64(b, 0)
            .zip(be_i32(b, 8))
            .zip(be_i32(b, 12))
            .map(|((us, days), months)| Value::Interval(interval_text(months, days, us))),
        Type::UUID => (b.len() == 16).then(|| Value::Uuid(uuid_from_bytes(b))),
        Type::JSON => Some(Value::Json(String::from_utf8_lossy(b).into_owned())),
        Type::JSONB => b
            .split_first()
            .map(|(_, rest)| Value::Json(String::from_utf8_lossy(rest).into_owned())),
        Type::JSONPATH => b.split_first().map(|(_, rest)| other(String::from_utf8_lossy(rest).into_owned())),
        Type::XML => Some(other(String::from_utf8_lossy(b).into_owned())),
        Type::INET | Type::CIDR => decode_inet(b).map(other),
        Type::MACADDR | Type::MACADDR8 => Some(other(b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":"))),
        Type::BIT | Type::VARBIT => decode_bits(b).map(other),
        Type::POINT => point(b, 0).map(other),
        Type::BOX => point(b, 0).zip(point(b, 16)).map(|(a, c)| other(format!("{a},{c}"))),
        Type::LSEG => point(b, 0).zip(point(b, 16)).map(|(a, c)| other(format!("[{a},{c}]"))),
        Type::LINE => be_f64(b, 0)
            .zip(be_f64(b, 8))
            .zip(be_f64(b, 16))
            .map(|((a, bb), c)| other(format!("{{{a},{bb},{c}}}"))),
        Type::CIRCLE => point(b, 0).zip(be_f64(b, 16)).map(|(p, r)| other(format!("<{p},{r}>"))),
        Type::PATH => {
            let closed = b.first().copied().unwrap_or(0) != 0;
            points(b, 1).map(|p| other(if closed { format!("({p})") } else { format!("[{p}]") }))
        }
        Type::POLYGON => points(b, 0).map(|p| other(format!("({p})"))),
        _ => None,
    };
    v.unwrap_or_else(|| match ty.name() {
        "citext" => Value::Text(String::from_utf8_lossy(b).into_owned()),
        // Version byte, then text.
        "ltree" | "lquery" | "ltxtquery" => other(String::from_utf8_lossy(b.get(1..).unwrap_or(b)).into_owned()),
        "hstore" => decode_hstore(b).map(other).unwrap_or_else(|| fallback(ty, b)),
        _ => fallback(ty, b),
    })
}

/// Unknown binary format: the text when it is clean UTF-8, else hex.
fn fallback(ty: &Type, b: &[u8]) -> Value {
    let text = match std::str::from_utf8(b) {
        Ok(s) if !s.chars().any(|c| c.is_control() && !c.is_whitespace()) => s.to_owned(),
        _ => crate::engines::common::value::hex(b),
    };
    Value::Other {
        type_name: ty.name().to_owned(),
        text,
    }
}

fn time_of_micros(us: i64) -> Option<NaiveTime> {
    if us == 86_400_000_000 {
        return NaiveTime::from_hms_opt(23, 59, 59); // 24:00:00 is allowed by PostgreSQL; shown as the end of the day
    }
    NaiveTime::from_num_seconds_from_midnight_opt((us / 1_000_000) as u32, ((us % 1_000_000) * 1000) as u32)
}

fn offset_text(east_seconds: i32) -> String {
    let sign = if east_seconds < 0 { '-' } else { '+' };
    let s = east_seconds.abs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    match (m, sec) {
        (0, 0) => format!("{sign}{h:02}"),
        (_, 0) => format!("{sign}{h:02}:{m:02}"),
        _ => format!("{sign}{h:02}:{m:02}:{sec:02}"),
    }
}

/// PostgreSQL's default (`postgres`) interval style: `1 year 2 mons 3 days 04:05:06.5`.
pub fn interval_text(months: i32, days: i32, micros: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let (y, mo) = (months / 12, months % 12);
    let plural = |n: i64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if y != 0 {
        parts.push(plural(y.into(), "year", "years"));
    }
    if mo != 0 {
        parts.push(plural(mo.into(), "mon", "mons"));
    }
    if days != 0 {
        parts.push(plural(days.into(), "day", "days"));
    }
    if micros != 0 || parts.is_empty() {
        let neg = micros < 0;
        let us = micros.unsigned_abs();
        let (h, rem) = (us / 3_600_000_000, us % 3_600_000_000);
        let (m, rem) = (rem / 60_000_000, rem % 60_000_000);
        let (s, frac) = (rem / 1_000_000, rem % 1_000_000);
        let mut t = format!("{}{h:02}:{m:02}:{s:02}", if neg { "-" } else { "" });
        if frac != 0 {
            let f = format!("{frac:06}");
            let _ = write!(t, ".{}", f.trim_end_matches('0'));
        }
        parts.push(t);
    }
    parts.join(" ")
}

/// `value / 10^scale` as plain decimal text.
fn scaled(value: i128, scale: usize) -> String {
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let s = if scale == 0 {
        digits
    } else if digits.len() > scale {
        format!("{}.{}", &digits[..digits.len() - scale], &digits[digits.len() - scale..])
    } else {
        format!("0.{}{}", "0".repeat(scale - digits.len()), digits)
    };
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

/// NUMERIC binary: ndigits, weight, sign, dscale, then base-10000 digits.
pub fn decode_numeric(b: &[u8]) -> Option<String> {
    let ndigits = be_i16(b, 0)? as usize;
    let weight = be_i16(b, 2)? as i32;
    let sign = be_i16(b, 4)? as u16;
    let dscale = be_i16(b, 6)? as usize;
    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let mut digits = Vec::with_capacity(ndigits);
    for i in 0..ndigits {
        digits.push(be_i16(b, 8 + 2 * i)? as u16);
    }
    let mut int_part = String::new();
    for i in 0..=weight.max(-1) {
        let d = digits.get(i as usize).copied().unwrap_or(0);
        if int_part.is_empty() {
            if d != 0 {
                int_part = d.to_string();
            }
        } else {
            let _ = write!(int_part, "{d:04}");
        }
    }
    if int_part.is_empty() {
        int_part.push('0');
    }
    let mut frac = String::new();
    let mut i = weight + 1;
    while frac.len() < dscale {
        let d = if i < 0 { 0 } else { digits.get(i as usize).copied().unwrap_or(0) };
        let _ = write!(frac, "{d:04}");
        i += 1;
    }
    frac.truncate(dscale);
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    s.push_str(&int_part);
    if dscale > 0 {
        s.push('.');
        s.push_str(&frac);
    }
    Some(s)
}

fn decode_array(elem: &Type, b: &[u8]) -> Option<Value> {
    let ndim = be_i32(b, 0)? as usize;
    if ndim == 0 {
        return Some(Value::Array(Vec::new()));
    }
    let mut dims = Vec::with_capacity(ndim);
    for d in 0..ndim {
        dims.push(be_i32(b, 12 + 8 * d)? as usize);
    }
    let mut at = 12 + 8 * ndim;
    let total: usize = dims.iter().product();
    let mut flat = Vec::with_capacity(total);
    for _ in 0..total {
        let len = be_i32(b, at)?;
        at += 4;
        if len < 0 {
            flat.push(Value::Null);
        } else {
            let len = len as usize;
            flat.push(decode(elem, b.get(at..at + len)?));
            at += len;
        }
    }
    // Rebuild nesting for multi-dimensional arrays (innermost dimension last).
    for &d in dims.iter().skip(1).rev() {
        flat = flat.chunks(d.max(1)).map(|c| Value::Array(c.to_vec())).collect();
    }
    Some(Value::Array(flat))
}

fn element_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Text(s) => {
            if s.is_empty() || s.contains([',', '(', ')', '[', ']', '"', ' ', '\\']) {
                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                s.clone()
            }
        }
        Value::Timestamp(_) | Value::TimestampTz(_) => format!("\"{}\"", v.display().unwrap_or_default()),
        other => other.display().unwrap_or_default(),
    }
}

fn decode_range(sub: &Type, b: &[u8]) -> Option<String> {
    let flags = *b.first()?;
    if flags & 0x01 != 0 {
        return Some("empty".into());
    }
    let mut at = 1;
    let mut bound = |present: bool| -> Option<String> {
        if !present {
            return Some(String::new());
        }
        let len = be_i32(b, at)? as usize;
        at += 4;
        let v = decode(sub, b.get(at..at + len)?);
        at += len;
        Some(element_text(&v))
    };
    let lower = bound(flags & 0x08 == 0)?;
    let upper = bound(flags & 0x10 == 0)?;
    Some(format!(
        "{}{lower},{upper}{}",
        if flags & 0x02 != 0 { '[' } else { '(' },
        if flags & 0x04 != 0 { ']' } else { ')' }
    ))
}

fn decode_composite(fields: &[tokio_postgres::types::Field], b: &[u8]) -> Option<String> {
    let n = be_i32(b, 0)? as usize;
    let mut at = 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        at += 4; // field type oid
        let len = be_i32(b, at)?;
        at += 4;
        if len < 0 {
            out.push(String::new());
            continue;
        }
        let len = len as usize;
        let bytes = b.get(at..at + len)?;
        at += len;
        let v = match fields.get(i) {
            Some(f) => decode(f.type_(), bytes),
            None => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        };
        out.push(element_text(&v));
    }
    Some(format!("({})", out.join(",")))
}

fn decode_inet(b: &[u8]) -> Option<String> {
    let (family, bits, is_cidr, nb) = (*b.first()?, *b.get(1)?, *b.get(2)? != 0, *b.get(3)? as usize);
    let addr = b.get(4..4 + nb)?;
    let (text, full) = if family == 2 {
        (addr.iter().map(u8::to_string).collect::<Vec<_>>().join("."), 32)
    } else {
        let a: [u8; 16] = addr.try_into().ok()?;
        (std::net::Ipv6Addr::from(a).to_string(), 128)
    };
    Some(if is_cidr || bits != full { format!("{text}/{bits}") } else { text })
}

fn decode_bits(b: &[u8]) -> Option<String> {
    let len = be_i32(b, 0)? as usize;
    let data = b.get(4..)?;
    Some(
        (0..len)
            .map(|i| {
                if data.get(i / 8).is_some_and(|byte| byte & (0x80 >> (i % 8)) != 0) {
                    '1'
                } else {
                    '0'
                }
            })
            .collect(),
    )
}

fn point(b: &[u8], at: usize) -> Option<String> {
    Some(format!("({},{})", format_float(be_f64(b, at)?), format_float(be_f64(b, at + 8)?)))
}

fn points(b: &[u8], at: usize) -> Option<String> {
    let n = be_i32(b, at)? as usize;
    (0..n)
        .map(|i| point(b, at + 4 + 16 * i))
        .collect::<Option<Vec<_>>>()
        .map(|v| v.join(","))
}

fn decode_hstore(b: &[u8]) -> Option<String> {
    let n = be_i32(b, 0)? as usize;
    let mut at = 4;
    let mut parts = Vec::with_capacity(n);
    let take = |at: &mut usize| -> Option<Option<String>> {
        let len = be_i32(b, *at)?;
        *at += 4;
        if len < 0 {
            return Some(None);
        }
        let s = String::from_utf8_lossy(b.get(*at..*at + len as usize)?).into_owned();
        *at += len as usize;
        Some(Some(s))
    };
    for _ in 0..n {
        let k = take(&mut at)??;
        let v = take(&mut at)?;
        parts.push(format!(
            "\"{k}\"=>{}",
            v.map(|v| format!("\"{v}\"")).unwrap_or_else(|| "NULL".into())
        ));
    }
    Some(parts.join(", "))
}

/// The text form of a value as PostgreSQL's input functions read it (bound as text and cast server-side).
pub fn to_param_text(v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => return None,
        Value::Bytes(b) => format!("\\x{}", &crate::engines::common::value::hex(b)[2..]),
        Value::TimestampTz(t) => t.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string(),
        Value::Array(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|i| match i {
                    Value::Null => "NULL".to_owned(),
                    Value::Array(_) => to_param_text(i).unwrap_or_default(),
                    other => {
                        let t = to_param_text(other).unwrap_or_default();
                        format!("\"{}\"", t.replace('\\', "\\\\").replace('"', "\\\""))
                    }
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        other => other.display().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric(ndigits: i16, weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [ndigits, weight, sign as i16, dscale] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        for d in digits {
            b.extend_from_slice(&d.to_be_bytes());
        }
        b
    }

    #[test]
    fn numeric_binary_to_text() {
        // 12345678901234.123456 = 12 3456 7890 1234 . 1234 5600, weight 3
        assert_eq!(
            decode_numeric(&numeric(6, 3, 0, 6, &[12, 3456, 7890, 1234, 1234, 5600])).unwrap(),
            "12345678901234.123456"
        );
        assert_eq!(decode_numeric(&numeric(1, -1, 0x4000, 4, &[5])).unwrap(), "-0.0005");
        assert_eq!(decode_numeric(&numeric(0, 0, 0, 2, &[])).unwrap(), "0.00");
        assert_eq!(decode_numeric(&numeric(1, 1, 0, 0, &[1])).unwrap(), "10000");
        assert_eq!(decode_numeric(&numeric(0, 0, 0xC000, 0, &[])).unwrap(), "NaN");
    }

    #[test]
    fn interval_and_scaled_text() {
        assert_eq!(interval_text(14, 3, 14_706_000_000), "1 year 2 mons 3 days 04:05:06");
        assert_eq!(interval_text(0, 0, 0), "00:00:00");
        assert_eq!(interval_text(0, -1, -1_500_000), "-1 days -00:00:01.5");
        assert_eq!(scaled(1999, 2), "19.99");
        assert_eq!(scaled(-5, 2), "-0.05");
    }

    #[test]
    fn param_text_forms() {
        assert_eq!(to_param_text(&Value::Bytes(vec![0xde, 0xad])).unwrap(), "\\xdead");
        let arr = Value::Array(vec![Value::Text("y z".into()), Value::Null, Value::Text("a\"b".into())]);
        assert_eq!(to_param_text(&arr).unwrap(), r#"{"y z",NULL,"a\"b"}"#);
        assert_eq!(to_param_text(&Value::Null), None);
    }

    #[test]
    fn decodes_builtins() {
        assert_eq!(decode(&Type::INT4, &7i32.to_be_bytes()), Value::Int(7));
        assert_eq!(decode(&Type::DATE, &0i32.to_be_bytes()).display().unwrap(), "2000-01-01");
        assert_eq!(decode(&Type::DATE, &i32::MAX.to_be_bytes()).display().unwrap(), "infinity");
        assert_eq!(
            decode(&Type::INET, &[2, 32, 0, 4, 192, 168, 1, 10]).display().unwrap(),
            "192.168.1.10"
        );
        assert_eq!(decode(&Type::CIDR, &[2, 8, 1, 4, 10, 0, 0, 0]).display().unwrap(), "10.0.0.0/8");
        assert_eq!(decode(&Type::BIT, &[0, 0, 0, 8, 0xAA]).display().unwrap(), "10101010");
        assert_eq!(decode(&Type::JSONB, b"\x01{\"a\": 1}"), Value::Json("{\"a\": 1}".into()));
        let mut arr = Vec::new();
        for v in [1i32, 0, 23, 2, 1] {
            arr.extend_from_slice(&v.to_be_bytes()); // ndim 1, no nulls, int4, dim 2, lbound 1
        }
        for v in [5i32, 6] {
            arr.extend_from_slice(&4i32.to_be_bytes());
            arr.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(decode(&Type::INT4_ARRAY, &arr), Value::Array(vec![Value::Int(5), Value::Int(6)]));
    }
}
