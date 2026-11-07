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

convert_from!(
    std::io::Error,
    serde_json::Error,
    mysql_async::Error,
    tiberius::error::Error,
    mongodb::error::Error,
    reqwest::Error,
);

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
}
