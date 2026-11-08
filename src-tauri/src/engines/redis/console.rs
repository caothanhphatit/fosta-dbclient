//! A redis-cli style console: tokenizes command lines the way `redis-cli` does, runs them on a dedicated connection with a
//! timeout, and returns typed replies (RESP2 and RESP3).

use std::time::{Duration, Instant};

use redis::{aio::MultiplexedConnection, Value};
use serde::{Deserialize, Serialize};

use super::{
    classify::{classify, Classification, CommandClass},
    connection::{with_timeout, RedisEngine},
    types::Blob,
};
use crate::error::{AppError, AppResult};

/// Replies with more elements than this are cut (the rest is counted in `RedisReply::Truncated`).
const MAX_REPLY_ITEMS: usize = 10_000;

/// Splits one command line into arguments like `redis-cli`:
/// - whitespace separates arguments;
/// - `"double quotes"` allow `\n \r \t \b \a \\ \"` and `\xHH` escapes;
/// - `'single quotes'` are literal except `\'`;
/// - a closing quote must be followed by whitespace or the end of the line.
pub fn tokenize(line: &str) -> AppResult<Vec<Vec<u8>>> {
    let bytes = line.as_bytes();
    let mut args = Vec::new();
    let mut index = 0;
    let unbalanced = || AppError::new("Invalid argument(s): unbalanced quotes.");
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            return Ok(args);
        }
        let mut current = Vec::new();
        let mut in_double = false;
        let mut in_single = false;
        loop {
            if in_double {
                let Some(&byte) = bytes.get(index) else {
                    return Err(unbalanced());
                };
                if byte == b'\\'
                    && index + 3 < bytes.len()
                    && bytes[index + 1] == b'x'
                    && is_hex(bytes[index + 2])
                    && is_hex(bytes[index + 3])
                {
                    current.push(hex_value(bytes[index + 2]) * 16 + hex_value(bytes[index + 3]));
                    index += 3;
                } else if byte == b'\\' && index + 1 < bytes.len() {
                    index += 1;
                    current.push(match bytes[index] {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'b' => 0x08,
                        b'a' => 0x07,
                        other => other,
                    });
                } else if byte == b'"' {
                    if bytes.get(index + 1).is_some_and(|next| !next.is_ascii_whitespace()) {
                        return Err(unbalanced());
                    }
                    index += 1;
                    break;
                } else {
                    current.push(byte);
                }
            } else if in_single {
                let Some(&byte) = bytes.get(index) else {
                    return Err(unbalanced());
                };
                if byte == b'\\' && bytes.get(index + 1) == Some(&b'\'') {
                    index += 1;
                    current.push(b'\'');
                } else if byte == b'\'' {
                    if bytes.get(index + 1).is_some_and(|next| !next.is_ascii_whitespace()) {
                        return Err(unbalanced());
                    }
                    index += 1;
                    break;
                } else {
                    current.push(byte);
                }
            } else {
                match bytes.get(index) {
                    None => break,
                    Some(byte) if byte.is_ascii_whitespace() => break,
                    Some(b'"') => in_double = true,
                    Some(b'\'') => in_single = true,
                    Some(&byte) => current.push(byte),
                }
            }
            index += 1;
        }
        args.push(current);
    }
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

fn hex_value(byte: u8) -> u8 {
    (byte as char).to_digit(16).unwrap_or(0) as u8
}

/// Splits console input into command lines: one command per line, blank lines and `#` / `//` comments skipped.
/// Returns `(1-based line number, line)`.
pub fn split_lines(input: &str) -> Vec<(usize, &str)> {
    input
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#') && !line.starts_with("//"))
        .collect()
}

/// A typed reply. `kind` names the variant in JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RedisReply {
    Nil,
    Integer {
        value: i64,
    },
    Bulk {
        value: Blob,
    },
    Simple {
        value: String,
    },
    Ok,
    Double {
        value: f64,
    },
    Boolean {
        value: bool,
    },
    BigNumber {
        value: String,
    },
    Verbatim {
        format: String,
        text: String,
    },
    Array {
        items: Vec<RedisReply>,
    },
    Set {
        items: Vec<RedisReply>,
    },
    Map {
        entries: Vec<(RedisReply, RedisReply)>,
    },
    Push {
        push_kind: String,
        items: Vec<RedisReply>,
    },
    Error {
        code: String,
        message: Option<String>,
    },
    /// Stands in for the elements cut after [`MAX_REPLY_ITEMS`].
    Truncated {
        omitted: usize,
    },
}

impl RedisReply {
    pub fn from_value(value: Value) -> Self {
        fn list(items: Vec<Value>) -> Vec<RedisReply> {
            let total = items.len();
            let mut out: Vec<RedisReply> = items.into_iter().take(MAX_REPLY_ITEMS).map(RedisReply::from_value).collect();
            if total > MAX_REPLY_ITEMS {
                out.push(RedisReply::Truncated {
                    omitted: total - MAX_REPLY_ITEMS,
                });
            }
            out
        }
        match value {
            Value::Nil => Self::Nil,
            Value::Int(value) => Self::Integer { value },
            Value::BulkString(bytes) => Self::Bulk { value: Blob(bytes) },
            Value::SimpleString(value) => Self::Simple { value },
            Value::Okay => Self::Ok,
            Value::Double(value) => Self::Double { value },
            Value::Boolean(value) => Self::Boolean { value },
            Value::BigNumber(value) => Self::BigNumber {
                value: String::from_utf8_lossy(&value).into_owned(),
            },
            Value::VerbatimString { format, text } => Self::Verbatim {
                format: format.to_string(),
                text,
            },
            Value::Array(items) => Self::Array { items: list(items) },
            Value::Set(items) => Self::Set { items: list(items) },
            Value::Map(pairs) => Self::Map {
                entries: pairs
                    .into_iter()
                    .take(MAX_REPLY_ITEMS)
                    .map(|(k, v)| (Self::from_value(k), Self::from_value(v)))
                    .collect(),
            },
            Value::Attribute { data, .. } => Self::from_value(*data),
            Value::Push { kind, data } => Self::Push {
                push_kind: format!("{kind:?}"),
                items: list(data),
            },
            Value::ServerError(error) => Self::Error {
                code: error.code().to_string(),
                message: error.details().map(str::to_string),
            },
            other => Self::Simple {
                value: format!("{other:?}"),
            },
        }
    }

    /// redis-cli style text rendering (used by the console's text mode and by tests).
    pub fn to_cli_text(&self) -> String {
        let mut out = String::new();
        render(self, 0, &mut out);
        out
    }
}

fn render(reply: &RedisReply, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match reply {
        RedisReply::Nil => out.push_str("(nil)"),
        RedisReply::Integer { value } => out.push_str(&format!("(integer) {value}")),
        RedisReply::Bulk { value } => match value.as_str() {
            Some(text) => out.push_str(&format!("{text:?}")),
            None => out.push_str(&format!(
                "\"{}\"",
                value.0.iter().map(|b| format!("\\x{b:02x}")).collect::<String>()
            )),
        },
        RedisReply::Simple { value } => out.push_str(value),
        RedisReply::Ok => out.push_str("OK"),
        RedisReply::Double { value } => out.push_str(&format!("(double) {value}")),
        RedisReply::Boolean { value } => out.push_str(&format!("({value})")),
        RedisReply::BigNumber { value } => out.push_str(&format!("(big number) {value}")),
        RedisReply::Verbatim { text, .. } => out.push_str(text),
        RedisReply::Error { code, message } => out.push_str(&format!(
            "(error) {code}{}",
            message.as_deref().map(|m| format!(" {m}")).unwrap_or_default()
        )),
        RedisReply::Truncated { omitted } => out.push_str(&format!("... {omitted} more")),
        RedisReply::Array { items } | RedisReply::Set { items } | RedisReply::Push { items, .. } => {
            if items.is_empty() {
                out.push_str("(empty array)");
            }
            for (number, item) in items.iter().enumerate() {
                if number > 0 {
                    out.push('\n');
                    out.push_str(&pad);
                }
                let label = format!("{}) ", number + 1);
                out.push_str(&label);
                render(item, indent + label.len(), out);
            }
        }
        RedisReply::Map { entries } => {
            if entries.is_empty() {
                out.push_str("(empty hash)");
            }
            for (number, (key, value)) in entries.iter().enumerate() {
                if number > 0 {
                    out.push('\n');
                    out.push_str(&pad);
                }
                let label = format!("{}# ", number + 1);
                out.push_str(&label);
                render(key, indent + label.len(), out);
                out.push_str(" => ");
                render(value, indent + label.len() + 4, out);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleResult {
    /// 1-based line in the console input.
    pub line: usize,
    pub command: String,
    pub classification: Classification,
    /// The reply, or a server error reply (`RedisReply::Error`).
    pub reply: Option<RedisReply>,
    /// A client-side failure (timeout, refused command, parse error).
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

/// A console session with its own connection, so `SELECT`, `MULTI` and client settings stay in this console.
pub struct RedisConsole {
    conn: MultiplexedConnection,
    timeout: Duration,
    /// Commands queued since `MULTI`; `EXEC` is classified as the highest of them.
    queued: Option<Vec<CommandClass>>,
    /// `CLIENT ID` of this connection, so another connection can `CLIENT UNBLOCK` or kill what it runs.
    client_id: Option<i64>,
}

impl RedisEngine {
    pub async fn open_console(&self) -> AppResult<RedisConsole> {
        let mut conn = self.new_connection().await?;
        let client_id = redis::cmd("CLIENT")
            .arg("ID")
            .query_async::<Value>(&mut conn)
            .await
            .ok()
            .as_ref()
            .and_then(super::types::as_i64);
        Ok(RedisConsole {
            conn,
            timeout: self.command_timeout(),
            queued: None,
            client_id,
        })
    }
}

impl RedisConsole {
    pub fn client_id(&self) -> Option<i64> {
        self.client_id
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout.max(Duration::from_millis(100));
    }

    /// Runs one line for a contract session: the classification and the typed reply, or the error with its code kept.
    /// A server error reply (`WRONGTYPE`) may come back as `Ok(RedisReply::Error)`; the session turns it into an error.
    pub async fn exec_line(&mut self, line: &str) -> AppResult<(Classification, RedisReply)> {
        let args = tokenize(line)?;
        if args.is_empty() {
            return Err(AppError::new("Empty command."));
        }
        let classification = self.classify_args(&args);
        if classification.class == CommandClass::Unsupported {
            return Err(AppError::new(
                classification
                    .reason
                    .clone()
                    .unwrap_or_else(|| "The console cannot run this command.".into()),
            ));
        }
        let mut cmd = redis::cmd(&String::from_utf8_lossy(&args[0]));
        for arg in &args[1..] {
            cmd.arg(arg.as_slice());
        }
        let mut conn = self.conn.clone();
        let reply = with_timeout(self.timeout, conn.send_packed_command(&cmd)).await;
        self.track_transaction(&classification, &reply);
        Ok((classification, RedisReply::from_value(reply?)))
    }

    /// Classifies a line without running it (for the gateway's preview).
    pub fn classify_line(&self, line: &str) -> AppResult<Classification> {
        let args = tokenize(line)?;
        Ok(self.classify_args(&args))
    }

    fn classify_args(&self, args: &[Vec<u8>]) -> Classification {
        let mut classification = classify(args);
        if classification.command == "EXEC" {
            if let Some(highest) = self.queued.as_ref().and_then(|queued| queued.iter().max().copied()) {
                classification.class = classification.class.max(highest);
            }
        }
        classification
    }

    /// Runs every line of `input` in order and stops after the first client-side failure. Server error replies do not
    /// stop the run, like redis-cli.
    pub async fn run(&mut self, input: &str) -> Vec<ConsoleResult> {
        let mut results = Vec::new();
        for (line_number, line) in split_lines(input) {
            let result = self.run_line(line_number, line).await;
            let failed = result.error.is_some();
            results.push(result);
            if failed {
                break;
            }
        }
        results
    }

    pub async fn run_line(&mut self, line_number: usize, line: &str) -> ConsoleResult {
        let started = Instant::now();
        let done = |classification: Classification, reply: Option<RedisReply>, error: Option<String>| ConsoleResult {
            line: line_number,
            command: line.to_string(),
            classification,
            reply,
            error,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        let args = match tokenize(line) {
            Ok(args) if !args.is_empty() => args,
            Ok(_) => return done(classify(&[]), None, Some("Empty command.".into())),
            Err(error) => return done(classify(&[]), None, Some(error.to_string())),
        };
        let classification = self.classify_args(&args);
        if classification.class == CommandClass::Unsupported {
            let reason = classification.reason.clone();
            return done(classification, None, reason);
        }
        let mut cmd = redis::cmd(&String::from_utf8_lossy(&args[0]));
        for arg in &args[1..] {
            cmd.arg(arg.as_slice());
        }
        let mut conn = self.conn.clone();
        let reply = with_timeout(self.timeout, conn.send_packed_command(&cmd)).await;
        self.track_transaction(&classification, &reply);
        match reply {
            Ok(value) => done(classification, Some(RedisReply::from_value(value)), None),
            // Server errors come back as `Value::ServerError` replies; `Err` means the connection failed or timed out.
            Err(error) => done(classification, None, Some(error.to_string())),
        }
    }

    fn track_transaction(&mut self, classification: &Classification, reply: &AppResult<Value>) {
        match classification.command.as_str() {
            "MULTI" if reply.is_ok() => self.queued = Some(Vec::new()),
            "EXEC" | "DISCARD" | "RESET" => self.queued = None,
            _ => {
                if let Some(queued) = &mut self.queued {
                    queued.push(classification.class);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        tokenize(line)
            .unwrap()
            .into_iter()
            .map(|arg| String::from_utf8_lossy(&arg).into_owned())
            .collect()
    }

    #[test]
    fn tokenizes_like_redis_cli() {
        assert_eq!(words("  SET  key   value "), ["SET", "key", "value"]);
        assert_eq!(
            words(r#"set "hello world" 'it''s'"#.replace("''", "\\'").as_str()),
            ["set", "hello world", "it's"]
        );
        assert_eq!(words(r#"set k "a\nb\t\"q\"""#), ["set", "k", "a\nb\t\"q\""]);
        assert_eq!(tokenize(r#"set k "\x41\xff""#).unwrap()[2], vec![0x41, 0xff]);
        assert_eq!(words(r#"set k 'no \n escapes'"#), ["set", "k", "no \\n escapes"]);
        assert_eq!(words(r#"set k """#), ["set", "k", ""]);
        // A quote inside a word opens a quoted part, as in redis-cli.
        assert_eq!(words(r#"get a"b c""#), ["get", "ab c"]);
        assert!(tokenize(r#"set "unterminated"#).is_err());
        assert!(tokenize(r#"set "a"b"#).is_err());
        assert!(tokenize("set 'a").is_err());
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn splits_console_lines() {
        let lines = split_lines("# comment\nPING\n\n  // other\nGET a\n");
        assert_eq!(lines, vec![(2, "PING"), (5, "GET a")]);
    }

    #[test]
    fn renders_replies_like_redis_cli() {
        let reply = RedisReply::from_value(Value::Array(vec![
            Value::BulkString(b"a".to_vec()),
            Value::Int(3),
            Value::Array(vec![Value::Nil, Value::Okay]),
        ]));
        assert_eq!(reply.to_cli_text(), "1) \"a\"\n2) (integer) 3\n3) 1) (nil)\n   2) OK");
        let map = RedisReply::from_value(Value::Map(vec![(Value::SimpleString("k".into()), Value::Double(1.5))]));
        assert_eq!(map.to_cli_text(), "1# k => (double) 1.5");
        let json = serde_json::to_value(RedisReply::Integer { value: 1 }).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "integer", "value": 1 }));
    }
}
