use std::fmt;

use serde::{Serialize, Serializer};

pub const PASSWORD_REQUIRED: &str = "PASSWORD_REQUIRED";
pub const NOT_CONNECTED: &str = "Not connected. Double-click the connection to connect first.";

#[derive(Debug)]
pub struct AppError(String);

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

macro_rules! convert_from {
    ($($source:ty),* $(,)?) => {
        $(impl From<$source> for AppError {
            fn from(error: $source) -> Self {
                Self(error.to_string())
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
