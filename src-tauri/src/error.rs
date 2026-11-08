use std::fmt;

use serde::{Serialize, Serializer};

pub const PASSWORD_REQUIRED: &str = "PASSWORD_REQUIRED";
pub const NOT_CONNECTED: &str = "Not connected. Double-click the connection to connect first.";

/// An error shown to the user. Most errors are a sentence; some carry a structured
/// payload (a JSON object with a `code`, e.g. `CONFIRMATION_REQUIRED` from the statement
/// gateway) that the UI can act on. Plain errors serialise as a string, structured ones
/// as their payload, which always has a `message` too.
#[derive(Debug)]
pub struct AppError {
    message: String,
    detail: Option<serde_json::Value>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: None,
        }
    }

    /// An error with a structured payload; `message` is added to it when missing.
    pub fn structured(message: impl Into<String>, detail: impl Serialize) -> Self {
        let message = message.into();
        let mut detail = serde_json::to_value(detail).unwrap_or(serde_json::Value::Null);
        if let Some(object) = detail.as_object_mut() {
            object
                .entry("message")
                .or_insert_with(|| serde_json::Value::String(message.clone()));
        }
        Self {
            message,
            detail: Some(detail),
        }
    }

    /// The `code` of a structured error.
    pub fn code(&self) -> Option<&str> {
        self.detail.as_ref()?.get("code")?.as_str()
    }

    pub fn detail(&self) -> Option<&serde_json::Value> {
        self.detail.as_ref()
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
        match &self.detail {
            Some(detail) => detail.serialize(serializer),
            None => serializer.serialize_str(&self.message),
        }
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

convert_from!(std::io::Error, serde_json::Error, mongodb::error::Error, reqwest::Error);

impl From<mysql_async::Error> for AppError {
    fn from(error: mysql_async::Error) -> Self {
        // mysql_async nests its I/O errors, so the text reads "Input/output error: Input/output error: …".
        let text = error.to_string();
        const PREFIX: &str = "Input/output error: ";
        let mut rest = text.as_str();
        while rest.starts_with(PREFIX) && rest[PREFIX.len()..].starts_with(PREFIX) {
            rest = &rest[PREFIX.len()..];
        }
        Self::new(rest)
    }
}

/// Shown with SQL Server TLS failures: the usual cause is the self-signed certificate of a local or dev server.
pub const MSSQL_TLS_HINT: &str = "The server's TLS certificate is not trusted by this machine (SQL Server uses a self-signed certificate unless one \
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
                Self::new(message)
            }
            tiberius::error::Error::Tls(detail) => Self::new(tls_message(&detail)),
            other => Self::new(other.to_string()),
        }
    }
}

fn tls_message(detail: &str) -> String {
    let lower = detail.to_lowercase();
    let untrusted = ["not trusted", "untrusted", "unknownissuer", "unknown issuer", "self signed", "self-signed", "certificate chain", "-2146762487"]
        .iter()
        .any(|needle| lower.contains(needle));
    if untrusted {
        format!("Could not set up TLS with SQL Server: {detail}. {MSSQL_TLS_HINT}")
    } else {
        format!("Could not set up TLS with SQL Server: {detail}")
    }
}

#[cfg(feature = "desktop")]
convert_from!(keyring::Error);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_errors_stay_strings_and_structured_ones_are_objects() {
        assert_eq!(serde_json::to_value(AppError::new("boom")).unwrap(), "boom");
        let error = AppError::structured("Needs a confirmation", serde_json::json!({ "code": "X" }));
        assert_eq!(error.code(), Some("X"));
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "X");
        assert_eq!(value["message"], "Needs a confirmation");
        assert_eq!(error.to_string(), "Needs a confirmation");
    }

    #[test]
    fn mysql_io_errors_say_input_output_error_once() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused (os error 111)");
        let error = AppError::from(mysql_async::Error::Io(mysql_async::IoError::Io(io)));
        assert_eq!(error.to_string(), "Input/output error: Connection refused (os error 111)");
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
