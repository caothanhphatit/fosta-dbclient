//! The language service (ADR 0002 decision 4): completion schema, formatting and statement boundaries per language.
//! Placeholder for the completion and formatter work; `commands/lang.rs` calls into it.

/// Returns the text unchanged until a formatter is wired.
pub fn format_sql(_dialect: Option<crate::contract::SqlDialectId>, text: &str) -> String {
    text.to_owned()
}
