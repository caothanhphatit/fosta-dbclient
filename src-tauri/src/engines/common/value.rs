//! The neutral typed cell every engine maps its values to, its display form, and parsing of user-entered text back to a type.

use super::{EngineError, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::fmt::Write as _;

/// A typed cell. Integers wider than 64 bits and exact numerics travel as `Decimal` text so nothing is rounded.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Exact number (numeric, decimal, 128/256-bit and unsigned 64-bit integers) in plain decimal notation.
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(NaiveDateTime),
    TimestampTz(DateTime<FixedOffset>),
    /// An interval in the engine's own notation (`1 year 2 mons`, `+01 02:03:04.000000`).
    Interval(String),
    /// Lower-case, hyphenated.
    Uuid(String),
    /// JSON text as the server returned it (key order and number precision kept).
    Json(String),
    Array(Vec<Value>),
    /// Anything without a neutral type (ranges, geometry, network addresses, enums when not mapped to text, ...): the type name and
    /// the engine's text form.
    Other {
        type_name: String,
        text: String,
    },
}

/// The neutral type of a column, used to parse user input and to pick an editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "of")]
pub enum ValueKind {
    Bool,
    Int,
    Float,
    Decimal,
    Text,
    Bytes,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Uuid,
    Json,
    Array(Box<ValueKind>),
    Other,
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// A short name of the variant, used as the `t` tag over IPC.
    pub fn tag(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Decimal(_) => "decimal",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::Timestamp(_) => "timestamp",
            Value::TimestampTz(_) => "timestamptz",
            Value::Interval(_) => "interval",
            Value::Uuid(_) => "uuid",
            Value::Json(_) => "json",
            Value::Array(_) => "array",
            Value::Other { .. } => "other",
        }
    }

    /// The text shown in a grid cell; `None` for NULL. Binary is `0x…` hex (the app's convention), temporals are ISO 8601 with a
    /// space separator, arrays are JSON-like lists.
    pub fn display(&self) -> Option<String> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Decimal(s) | Value::Text(s) | Value::Interval(s) | Value::Uuid(s) | Value::Json(s) => s.clone(),
            Value::Bytes(b) => hex(b),
            Value::Date(d) => d.format("%Y-%m-%d").to_string(),
            Value::Time(t) => t.format("%H:%M:%S%.f").to_string(),
            Value::Timestamp(t) => t.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            Value::TimestampTz(t) => t.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string(),
            Value::Array(items) => {
                let mut out = String::from("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match item {
                        Value::Null => out.push_str("null"),
                        Value::Text(s) | Value::Uuid(s) | Value::Interval(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
                        Value::Date(_) | Value::Time(_) | Value::Timestamp(_) | Value::TimestampTz(_) | Value::Other { .. } => {
                            out.push_str(&serde_json::to_string(&item.display().unwrap_or_default()).unwrap_or_default())
                        }
                        other => out.push_str(&other.display().unwrap_or_default()),
                    }
                }
                out.push(']');
                out
            }
            Value::Other { text, .. } => text.clone(),
        })
    }

    /// Display text with NULL spelled out, for logs and tests.
    pub fn display_or_null(&self) -> String {
        self.display().unwrap_or_else(|| "NULL".to_owned())
    }
}

pub(crate) fn format_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        f.to_string()
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses `0x…` / `\x…` hex (or bare hex when `bare` is true) to bytes.
pub(crate) fn unhex(text: &str, bare: bool) -> Option<Vec<u8>> {
    let t = text.trim();
    let digits = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .or_else(|| t.strip_prefix("\\x"));
    let digits = match digits {
        Some(d) => d,
        None if bare => t,
        None => return None,
    };
    if digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Over IPC a value is `{ "t": tag, "v": payload }`. Integers outside ±2^53 and decimals are strings so JavaScript keeps every
/// digit; temporals are their display text; arrays nest values.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        struct Items<'a>(&'a [Value]);
        impl Serialize for Items<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
                for v in self.0 {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
        }
        const SAFE: i64 = 1 << 53;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("t", self.tag())?;
        match self {
            Value::Null => map.serialize_entry("v", &())?,
            Value::Bool(b) => map.serialize_entry("v", b)?,
            Value::Int(i) if (-SAFE..=SAFE).contains(i) => map.serialize_entry("v", i)?,
            Value::Float(f) if f.is_finite() => map.serialize_entry("v", f)?,
            Value::Array(items) => map.serialize_entry("v", &Items(items))?,
            Value::Other { type_name, text } => {
                map.serialize_entry("type", type_name)?;
                map.serialize_entry("v", text)?;
            }
            other => map.serialize_entry("v", &other.display().unwrap_or_default())?,
        }
        map.end()
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// Parsing user input

/// Parses what the user typed into a cell or a filter box as a value of `kind`. Empty text is an empty string for text columns
/// and an error for the others (NULL is chosen explicitly in the UI, never by an empty box).
pub fn parse_input(kind: &ValueKind, text: &str) -> Result<Value> {
    let t = text.trim();
    let bad = |what: &str| EngineError::input(format!("'{text}' is not a valid {what}."));
    Ok(match kind {
        ValueKind::Text | ValueKind::Other => {
            if matches!(kind, ValueKind::Other) {
                Value::Other {
                    type_name: String::new(),
                    text: text.to_owned(),
                }
            } else {
                Value::Text(text.to_owned())
            }
        }
        ValueKind::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "y" | "on" => Value::Bool(true),
            "false" | "f" | "0" | "no" | "n" | "off" => Value::Bool(false),
            _ => return Err(bad("boolean (true or false)")),
        },
        ValueKind::Int => match t.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) if is_integer_text(t) => Value::Decimal(t.trim_start_matches('+').to_owned()),
            Err(_) => return Err(bad("whole number")),
        },
        ValueKind::Float => match t.to_ascii_lowercase().as_str() {
            "nan" => Value::Float(f64::NAN),
            "inf" | "infinity" | "+infinity" => Value::Float(f64::INFINITY),
            "-inf" | "-infinity" => Value::Float(f64::NEG_INFINITY),
            _ => Value::Float(t.parse::<f64>().map_err(|_| bad("number"))?),
        },
        ValueKind::Decimal => {
            if is_decimal_text(t) {
                Value::Decimal(t.trim_start_matches('+').to_owned())
            } else {
                return Err(bad("number"));
            }
        }
        ValueKind::Bytes => Value::Bytes(unhex(t, false).unwrap_or_else(|| text.as_bytes().to_vec())),
        ValueKind::Date => Value::Date(NaiveDate::parse_from_str(t, "%Y-%m-%d").map_err(|_| bad("date (YYYY-MM-DD)"))?),
        ValueKind::Time => Value::Time(parse_time(t).ok_or_else(|| bad("time (HH:MM:SS)"))?),
        ValueKind::Timestamp => Value::Timestamp(parse_timestamp(t).ok_or_else(|| bad("timestamp (YYYY-MM-DD HH:MM:SS)"))?),
        ValueKind::TimestampTz => Value::TimestampTz(parse_timestamptz(t).ok_or_else(|| bad("timestamp with time zone"))?),
        ValueKind::Interval => {
            if t.is_empty() {
                return Err(bad("interval"));
            }
            Value::Interval(t.to_owned())
        }
        ValueKind::Uuid => Value::Uuid(normalize_uuid(t).ok_or_else(|| bad("UUID"))?),
        ValueKind::Json => {
            serde_json::from_str::<serde_json::Value>(t).map_err(|e| EngineError::input(format!("The JSON is not valid: {e}.")))?;
            Value::Json(t.to_owned())
        }
        ValueKind::Array(elem) => {
            let json: serde_json::Value = serde_json::from_str(t).map_err(|_| bad("list (a JSON array such as [1, 2])"))?;
            let serde_json::Value::Array(items) = json else {
                return Err(bad("list (a JSON array such as [1, 2])"));
            };
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(match item {
                    serde_json::Value::Null => Value::Null,
                    serde_json::Value::String(s) => parse_input(elem, &s)?,
                    other => parse_input(elem, &other.to_string())?,
                });
            }
            Value::Array(out)
        }
    })
}

fn is_integer_text(t: &str) -> bool {
    let d = t.strip_prefix(['-', '+']).unwrap_or(t);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn is_decimal_text(t: &str) -> bool {
    let s = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mut parts = mantissa.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let digits_ok =
        (!int.is_empty() || !frac.is_empty()) && int.bytes().all(|b| b.is_ascii_digit()) && frac.bytes().all(|b| b.is_ascii_digit());
    let exp_ok = exp.is_none_or(is_integer_text);
    digits_ok && exp_ok
}

pub(crate) fn parse_time(t: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(t, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M"))
        .ok()
}

pub(crate) fn parse_timestamp(t: &str) -> Option<NaiveDateTime> {
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"] {
        if let Ok(v) = NaiveDateTime::parse_from_str(t, f) {
            return Some(v);
        }
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d").ok().map(|d| d.and_time(NaiveTime::MIN))
}

/// Accepts RFC 3339, `YYYY-MM-DD HH:MM:SS[.f]±HH[:MM]`, and a bare timestamp (taken as UTC).
pub(crate) fn parse_timestamptz(t: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(v) = DateTime::parse_from_rfc3339(t) {
        return Some(v);
    }
    for f in ["%Y-%m-%d %H:%M:%S%.f%#z", "%Y-%m-%dT%H:%M:%S%.f%#z", "%Y-%m-%d %H:%M:%S%.f %#z"] {
        if let Ok(v) = DateTime::parse_from_str(t, f) {
            return Some(v);
        }
    }
    parse_timestamp(t).map(|n| n.and_utc().fixed_offset())
}

/// Decodes %XX escapes (connection strings).
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn normalize_uuid(t: &str) -> Option<String> {
    let hex: String = t.trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let h = hex.to_ascii_lowercase();
    Some(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

pub(crate) fn uuid_from_bytes(b: &[u8]) -> String {
    normalize_uuid(&hex(b)[2..]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_forms() {
        assert_eq!(Value::Null.display(), None);
        assert_eq!(Value::Bytes(vec![0xde, 0xad]).display().unwrap(), "0xdead");
        assert_eq!(Value::Float(f64::INFINITY).display().unwrap(), "Infinity");
        let ts = NaiveDate::from_ymd_opt(2026, 2, 28)
            .unwrap()
            .and_hms_micro_opt(13, 45, 30, 500_000)
            .unwrap();
        assert_eq!(Value::Timestamp(ts).display().unwrap(), "2026-02-28 13:45:30.500");
        let arr = Value::Array(vec![Value::Int(1), Value::Null, Value::Text("a\"b".into())]);
        assert_eq!(arr.display().unwrap(), r#"[1, null, "a\"b"]"#);
    }

    #[test]
    fn serializes_big_ints_as_strings() {
        assert_eq!(serde_json::to_string(&Value::Int(5)).unwrap(), r#"{"t":"int","v":5}"#);
        assert_eq!(
            serde_json::to_string(&Value::Int(i64::MAX)).unwrap(),
            r#"{"t":"int","v":"9223372036854775807"}"#
        );
        assert_eq!(serde_json::to_string(&Value::Null).unwrap(), r#"{"t":"null","v":null}"#);
    }

    #[test]
    fn parses_user_input_per_kind() {
        assert_eq!(parse_input(&ValueKind::Bool, " yes ").unwrap(), Value::Bool(true));
        assert_eq!(parse_input(&ValueKind::Int, "-42").unwrap(), Value::Int(-42));
        assert_eq!(
            parse_input(&ValueKind::Int, "18446744073709551615").unwrap(),
            Value::Decimal("18446744073709551615".into())
        );
        assert!(parse_input(&ValueKind::Int, "4.5").is_err());
        assert_eq!(parse_input(&ValueKind::Decimal, "+12.50").unwrap(), Value::Decimal("12.50".into()));
        assert!(parse_input(&ValueKind::Decimal, "12,5").is_err());
        assert_eq!(parse_input(&ValueKind::Bytes, "0xDEAD").unwrap(), Value::Bytes(vec![0xde, 0xad]));
        assert!(matches!(
            parse_input(&ValueKind::Timestamp, "2026-01-02T03:04:05.25").unwrap(),
            Value::Timestamp(_)
        ));
        assert!(matches!(
            parse_input(&ValueKind::Timestamp, "2026-01-02").unwrap(),
            Value::Timestamp(_)
        ));
        let tz = parse_input(&ValueKind::TimestampTz, "2026-01-02 03:04:05+07").unwrap();
        assert_eq!(tz.display().unwrap(), "2026-01-02 03:04:05+07:00");
        assert_eq!(
            parse_input(&ValueKind::Uuid, "{A0EEBC999C0B4EF8BB6D6BB9BD380A11}").unwrap(),
            Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into())
        );
        assert!(parse_input(&ValueKind::Json, "{bad").is_err());
        assert_eq!(
            parse_input(&ValueKind::Array(Box::new(ValueKind::Int)), "[1, null, \"3\"]").unwrap(),
            Value::Array(vec![Value::Int(1), Value::Null, Value::Int(3)])
        );
    }

    #[test]
    fn decimal_text_validation() {
        for ok in ["0", "-1.5", ".5", "5.", "1e10", "1.5E-3"] {
            assert!(is_decimal_text(ok), "{ok}");
        }
        for bad in ["", ".", "1.2.3", "abc", "1e", "--1"] {
            assert!(!is_decimal_text(bad), "{bad}");
        }
    }
}
