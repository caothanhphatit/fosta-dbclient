use std::fmt;

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

pub const PASSWORD_REQUIRED: &str = "PASSWORD_REQUIRED";
pub const NOT_CONNECTED: &str = "Not connected. Double-click the connection to connect first.";
/// The code of an error that has no more specific one.
pub const GENERIC: &str = "ERROR";

/// An error shown to the user, with one structured shape for every engine and command:
///
/// ```json
/// { "code": "ENGINE_ERROR", "message": "…", "engine": "mariadb", "detail": "…", "sqlstate": "42000",
///   "vendorCode": 1064, "position": { "line": 1, "column": 8, "offset": 7 },
///   "statement": { "index": 0, "text": "SELEC 1" }, "cause": ["…"], "retryable": false }
/// ```
///
/// Only `code` and `message` are always present (`code` is `"ERROR"` when nothing more specific applies). Some errors carry
/// more fields the UI acts on (the gateway's `CONFIRMATION_REQUIRED` has a token, `CONFLICT` lists rows). Errors are never
/// flattened to strings on the way to the UI, on the desktop or on the web.
#[derive(Debug, Clone)]
pub struct AppError {
    message: String,
    payload: Map<String, Value>,
}

pub type AppResult<T> = Result<T, AppError>;

/// Where in the statement text an error is, for the editor to jump to. `line` and `column` are 1-based; `offset` is
/// 0-based in characters from the start of the text that was run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPosition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl ErrorPosition {
    pub fn line(line: u32) -> Self {
        Self {
            line: Some(line),
            ..Self::default()
        }
    }

    /// Line and column of a 0-based character offset into `text`.
    pub fn at_offset(text: &str, offset: usize) -> Self {
        let mut line = 1u32;
        let mut column = 1u32;
        for ch in text.chars().take(offset) {
            if ch == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        Self {
            line: Some(line),
            column: Some(column),
            offset: Some(offset as u32),
        }
    }
}

impl AppError {
    /// A plain error (`code` "ERROR").
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            payload: Map::new(),
        }
    }

    /// An error with a specific code.
    pub fn coded(code: &str, message: impl Into<String>) -> Self {
        Self::new(message).with("code", code)
    }

    /// An error with a structured payload (an object with a `code`); `message` is added to it.
    pub fn structured(message: impl Into<String>, payload: impl Serialize) -> Self {
        let message = message.into();
        let payload = match serde_json::to_value(payload) {
            Ok(Value::Object(object)) => object,
            _ => Map::new(),
        };
        let mut error = Self { message, payload };
        error.payload.remove("message");
        error
    }

    /// Sets one field of the payload (overwrites).
    pub fn with(mut self, key: &str, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            if !value.is_null() {
                self.payload.insert(key.to_owned(), value);
            }
        }
        self
    }

    /// The engine id, unless one is already set.
    pub fn with_engine(mut self, engine: &str) -> Self {
        self.payload.entry("engine").or_insert_with(|| Value::String(engine.to_owned()));
        self
    }

    /// More text from the engine (PostgreSQL DETAIL, a hint, a panic message).
    pub fn with_detail(self, detail: impl Into<String>) -> Self {
        self.with("detail", detail.into())
    }

    pub fn with_sqlstate(self, sqlstate: &str) -> Self {
        if sqlstate.is_empty() {
            return self;
        }
        self.with("sqlstate", sqlstate)
    }

    /// The engine's own error number or name (MySQL 1064, SQL Server 208, Mongo 11000, Redis `WRONGTYPE`).
    pub fn with_vendor_code(self, code: impl Serialize) -> Self {
        self.with("vendorCode", code)
    }

    pub fn with_position(self, position: ErrorPosition) -> Self {
        self.with("position", position)
    }

    /// The failing statement of a script: its index and (truncated) text.
    pub fn with_statement(self, index: usize, text: &str) -> Self {
        let text = crate::logging::statement_excerpt(text);
        self.with("statement", serde_json::json!({ "index": index, "text": text }))
    }

    pub fn with_cause(self, cause: Vec<String>) -> Self {
        if cause.is_empty() {
            return self;
        }
        self.with("cause", cause)
    }

    pub fn retryable(self, retryable: bool) -> Self {
        self.with("retryable", retryable)
    }

    /// The `code`, when one was set.
    pub fn code(&self) -> Option<&str> {
        self.payload.get("code")?.as_str()
    }

    /// The structured payload (everything but `message`), when there is one.
    pub fn payload(&self) -> Option<Value> {
        (!self.payload.is_empty()).then(|| Value::Object(self.payload.clone()))
    }

    pub fn field(&self, key: &str) -> Option<&Value> {
        self.payload.get(key)
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = self.payload.clone();
        object.entry("code").or_insert_with(|| Value::String(GENERIC.to_owned()));
        object.insert("message".to_owned(), Value::String(self.message.clone()));
        object.serialize(serializer)
    }
}

macro_rules! convert_from {
    ($($source:ty),* $(,)?) => {
        $(impl From<$source> for AppError {
            fn from(error: $source) -> Self {
                Self::new(error.to_string())
            }
        })*
    };
}

convert_from!(std::io::Error, serde_json::Error);

impl From<reqwest::Error> for AppError {
    fn from(error: reqwest::Error) -> Self {
        let retryable = error.is_timeout() || error.is_connect();
        let mut cause = Vec::new();
        let mut source = std::error::Error::source(&error);
        while let Some(inner) = source {
            cause.push(inner.to_string());
            source = inner.source();
        }
        Self::new(error.to_string()).with_cause(cause).retryable(retryable)
    }
}

/// MySQL/MariaDB: server errors keep the error number and SQLSTATE; a syntax error's "at line N" becomes a position.
impl From<mysql_async::Error> for AppError {
    fn from(error: mysql_async::Error) -> Self {
        match error {
            mysql_async::Error::Server(server) => {
                // Deadlock, lock wait timeout, query interrupted by a timeout.
                let retryable = matches!(server.code, 1205 | 1213);
                let mut out = Self::coded("ENGINE_ERROR", server.message.clone())
                    .with_vendor_code(server.code)
                    .with_sqlstate(&server.state)
                    .retryable(retryable);
                if let Some(line) = mysql_error_line(&server.message) {
                    out = out.with_position(ErrorPosition::line(line));
                }
                out
            }
            mysql_async::Error::Io(_) => Self::coded("CONNECTION_ERROR", dedupe_io(&error.to_string())).retryable(true),
            other => Self::new(dedupe_io(&other.to_string())),
        }
    }
}

/// mysql_async nests its I/O errors, so the text reads "Input/output error: Input/output error: …".
fn dedupe_io(text: &str) -> String {
    const PREFIX: &str = "Input/output error: ";
    let mut rest = text;
    while rest.starts_with(PREFIX) && rest[PREFIX.len()..].starts_with(PREFIX) {
        rest = &rest[PREFIX.len()..];
    }
    rest.to_owned()
}

/// "… near 'x' at line 3" → 3.
fn mysql_error_line(message: &str) -> Option<u32> {
    let at = message.rfind("at line ")?;
    message[at + 8..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Shown with SQL Server TLS failures: the usual cause is the self-signed certificate of a local or dev server.
pub const MSSQL_TLS_HINT: &str =
    "The server's TLS certificate is not trusted by this machine (SQL Server uses a self-signed certificate unless one \
     is installed). Tick \"Trust server certificate\" in the connection dialog for a local or test server, or install a certificate \
     from a trusted authority on the server.";

impl From<tiberius::error::Error> for AppError {
    fn from(error: tiberius::error::Error) -> Self {
        match error {
            // The raw token error reads "Token error: '…' on server X executing  on line 1 (code: 102, state: 1, class: 15)".
            tiberius::error::Error::Server(token) => {
                let mut message = format!("{} (SQL Server error {}", token.message(), token.code());
                if token.line() > 0 {
                    message.push_str(&format!(", line {}", token.line()));
                }
                if !token.procedure().is_empty() {
                    message.push_str(&format!(", in {}", token.procedure()));
                }
                message.push(')');
                let mut out = Self::coded("ENGINE_ERROR", message)
                    .with_vendor_code(token.code())
                    .with("state", token.state())
                    .with("severity", token.class())
                    .retryable(matches!(token.code(), 1205 | 1222));
                if token.line() > 0 {
                    out = out.with_position(ErrorPosition::line(token.line()));
                }
                if !token.procedure().is_empty() {
                    out = out.with("procedure", token.procedure());
                }
                out
            }
            tiberius::error::Error::Tls(detail) => Self::coded("CONNECTION_ERROR", tls_message(&detail)),
            tiberius::error::Error::Io { .. } => Self::coded("CONNECTION_ERROR", error.to_string()).retryable(true),
            other => Self::new(other.to_string()),
        }
    }
}

fn tls_message(detail: &str) -> String {
    let lower = detail.to_lowercase();
    let untrusted = [
        "not trusted",
        "untrusted",
        "unknownissuer",
        "unknown issuer",
        "self signed",
        "self-signed",
        "certificate chain",
        "-2146762487",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if untrusted {
        format!("Could not set up TLS with SQL Server: {detail}. {MSSQL_TLS_HINT}")
    } else {
        format!("Could not set up TLS with SQL Server: {detail}")
    }
}

/// MongoDB: command and write errors keep `code`, `codeName` and `errInfo`; network errors are retryable.
impl From<mongodb::error::Error> for AppError {
    fn from(error: mongodb::error::Error) -> Self {
        use mongodb::error::{ErrorKind, WriteFailure};
        let retryable = error.contains_label("RetryableWriteError") || error.contains_label("TransientTransactionError");
        match error.kind.as_ref() {
            ErrorKind::Command(command) => Self::coded("ENGINE_ERROR", format!("{} ({})", command.message, command.code_name))
                .with_vendor_code(command.code)
                .with("codeName", &command.code_name)
                .retryable(retryable),
            ErrorKind::Write(WriteFailure::WriteError(write)) => {
                let name = write.code_name.clone().unwrap_or_default();
                let message = if name.is_empty() {
                    write.message.clone()
                } else {
                    format!("{} ({name})", write.message)
                };
                let mut out = Self::coded("ENGINE_ERROR", message)
                    .with_vendor_code(write.code)
                    .with("codeName", write.code_name.clone())
                    .retryable(retryable);
                if let Some(details) = &write.details {
                    out = out.with("errInfo", mongodb::bson::Bson::Document(details.clone()).into_relaxed_extjson());
                }
                out
            }
            ErrorKind::Write(WriteFailure::WriteConcernError(concern)) => {
                Self::coded("ENGINE_ERROR", format!("{} ({})", concern.message, concern.code_name))
                    .with_vendor_code(concern.code)
                    .with("codeName", &concern.code_name)
                    .retryable(true)
            }
            // insert_many: the first failed document's error, and its position.
            ErrorKind::InsertMany(failure) => match failure
                .write_errors
                .as_ref()
                .and_then(|errors| errors.iter().min_by_key(|e| e.index))
            {
                Some(write) => {
                    let name = write.code_name.clone().unwrap_or_default();
                    let message = if name.is_empty() {
                        write.message.clone()
                    } else {
                        format!("{} ({name})", write.message)
                    };
                    let mut out = Self::coded("ENGINE_ERROR", message)
                        .with_vendor_code(write.code)
                        .with("codeName", write.code_name.clone())
                        .with("index", write.index)
                        .retryable(retryable);
                    if let Some(details) = &write.details {
                        out = out.with("errInfo", mongodb::bson::Bson::Document(details.clone()).into_relaxed_extjson());
                    }
                    out
                }
                None => match &failure.write_concern_error {
                    Some(concern) => Self::coded("ENGINE_ERROR", format!("{} ({})", concern.message, concern.code_name))
                        .with_vendor_code(concern.code)
                        .with("codeName", &concern.code_name)
                        .retryable(true),
                    None => Self::new(error.to_string()).retryable(retryable),
                },
            },
            ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } | ErrorKind::ServerSelection { .. } => {
                Self::coded("CONNECTION_ERROR", error.to_string()).retryable(true)
            }
            ErrorKind::Authentication { message, .. } => Self::coded("CONNECTION_ERROR", format!("Authentication failed: {message}")),
            _ => Self::new(error.to_string()).retryable(retryable),
        }
    }
}

#[cfg(feature = "desktop")]
convert_from!(keyring::Error);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_serialises_as_an_object_with_code_and_message() {
        assert_eq!(
            serde_json::to_value(AppError::new("boom")).unwrap(),
            serde_json::json!({ "code": "ERROR", "message": "boom" })
        );
        let error = AppError::structured("Needs a confirmation", serde_json::json!({ "code": "X" }));
        assert_eq!(error.code(), Some("X"));
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "X");
        assert_eq!(value["message"], "Needs a confirmation");
        assert_eq!(error.to_string(), "Needs a confirmation");
        let rich = AppError::coded("ENGINE_ERROR", "bad")
            .with_engine("mariadb")
            .with_engine("ignored")
            .with_position(ErrorPosition::at_offset("SELECT\n  x", 9))
            .with_statement(2, "SELECT\n x");
        let value = serde_json::to_value(&rich).unwrap();
        assert_eq!(value["engine"], "mariadb");
        assert_eq!(value["position"], serde_json::json!({ "line": 2, "column": 3, "offset": 9 }));
        assert_eq!(value["statement"]["text"], "SELECT x");
    }

    #[test]
    fn mysql_server_errors_keep_number_sqlstate_and_line() {
        let error = AppError::from(mysql_async::Error::Server(mysql_async::ServerError {
            code: 1064,
            message: "You have an error in your SQL syntax; check the manual … near 'FORM t' at line 2".into(),
            state: "42000".into(),
        }));
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "ENGINE_ERROR");
        assert_eq!(value["vendorCode"], 1064);
        assert_eq!(value["sqlstate"], "42000");
        assert_eq!(value["position"]["line"], 2);
        assert_eq!(value["retryable"], false);
    }

    #[test]
    fn mysql_io_errors_say_input_output_error_once() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused (os error 111)");
        let error = AppError::from(mysql_async::Error::Io(mysql_async::IoError::Io(io)));
        assert_eq!(error.to_string(), "Input/output error: Connection refused (os error 111)");
        assert_eq!(error.code(), Some("CONNECTION_ERROR"));
    }

    #[test]
    fn untrusted_sql_server_certificates_explain_the_fix() {
        let error = AppError::from(tiberius::error::Error::Tls(
            "A certificate chain processed, but terminated in a root certificate which is not trusted by the trust provider. (os error -2146762487)"
                .into(),
        ));
        assert!(error.to_string().contains("Trust server certificate"), "{error}");
        let other = AppError::from(tiberius::error::Error::Tls("handshake eof".into()));
        assert!(!other.to_string().contains("Trust server certificate"));
    }
}
