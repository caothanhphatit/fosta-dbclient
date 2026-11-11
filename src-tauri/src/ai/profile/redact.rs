//! What a profile never keeps: system and credential objects (left out of the listing), the comments and defaults of
//! columns whose names look like credentials or personal data, and secret-looking text anywhere (API keys, tokens,
//! private keys). Engine-neutral: system containers are recognised by the `system` note engines put on their tree nodes
//! (MySQL `mysql`, `sys`…; MongoDB `admin`, `config`, `local`; SQL Server `master`…; ClickHouse `system`), plus names
//! every engine family shares.

use crate::contract::{NodeKind, ObjectNode};

/// The note engines put on system databases and schemas.
const SYSTEM_DETAIL: &str = "system";
/// Schemas that belong to the server whatever the engine says about them.
const SYSTEM_SCHEMAS: [&str; 6] = ["information_schema", "pg_catalog", "pg_toast", "sys", "performance_schema", "mysql"];
/// Catalog objects holding credentials (PostgreSQL), matched as name prefixes.
const CREDENTIAL_PREFIXES: [&str; 4] = ["pg_authid", "pg_shadow", "pg_user_mapping", "pg_auth_members"];
/// MongoDB `system.*` collections (`system.users`, `system.roles`, `system.profile`…).
const MONGO_SYSTEM_PREFIX: &str = "system.";
/// Elasticsearch / OpenSearch hidden and system indices (`.security-7`, `.kibana`, `.tasks`).
const HIDDEN_PREFIX: char = '.';

/// Comments, defaults and other engine text are cut to this many characters.
pub const MAX_TEXT_CHARS: usize = 500;
const REDACTED: &str = "[redacted]";

/// Words a column name contains to count as sensitive.
const SENSITIVE_PARTS: [&str; 8] = [
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "private",
    "credential",
];
/// Words a column name has as a whole word (`card_number`, `pin_hash`) to count as sensitive.
const SENSITIVE_WORDS: [&str; 7] = ["salt", "hash", "ssn", "card", "cvv", "iban", "pwd"];

/// A database node the assistant does not learn (a system database).
pub fn is_system_database(node: &ObjectNode) -> bool {
    node.detail.as_deref() == Some(SYSTEM_DETAIL)
}

/// A schema or folder whose objects the assistant does not learn.
pub fn is_excluded_container(node: &ObjectNode) -> bool {
    node.detail.as_deref() == Some(SYSTEM_DETAIL)
        || (node.kind == NodeKind::SCHEMA && SYSTEM_SCHEMAS.iter().any(|schema| schema.eq_ignore_ascii_case(node_name(node))))
}

/// An object the assistant does not learn: marked system, in a system schema, or a credential or hidden object.
pub fn is_excluded_object(node: &ObjectNode) -> bool {
    if node.detail.as_deref() == Some(SYSTEM_DETAIL) {
        return true;
    }
    let schema = node.object.as_ref().and_then(|object| object.schema.as_deref());
    if schema.is_some_and(|schema| SYSTEM_SCHEMAS.iter().any(|system| system.eq_ignore_ascii_case(schema))) {
        return true;
    }
    let name = node_name(node).to_ascii_lowercase();
    CREDENTIAL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || name.starts_with(MONGO_SYSTEM_PREFIX)
        || name.starts_with(HIDDEN_PREFIX)
}

fn node_name(node: &ObjectNode) -> &str {
    node.object.as_ref().map_or(node.label.as_str(), |object| object.name.as_str())
}

/// The words of a name: split at anything not a letter or digit and at lower-to-upper case changes (`cardNumber`).
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Whether a column called `name` may hold credentials or personal data.
pub fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_PARTS.iter().any(|part| lower.contains(part)) {
        return true;
    }
    let words = words(name);
    let joined = words.concat();
    joined.contains("apikey") || words.iter().any(|word| SENSITIVE_WORDS.contains(&word.as_str()))
}

/// Prefixes of well-known credentials (OpenAI, GitHub, Slack, AWS, Google, JWT, Stripe).
const SECRET_PREFIXES: [&str; 9] = ["sk-", "ghp_", "gho_", "github_pat_", "xox", "AKIA", "AIza", "eyJ", "sk_live_"];
const MIN_SECRET_CHARS: usize = 20;
const MIN_RANDOM_CHARS: usize = 32;

/// Whether one whitespace-separated word looks like a credential: a known key prefix, a long hex string, or a long
/// mix of upper case, lower case and digits (base64 or base62 tokens).
fn looks_like_secret(word: &str) -> bool {
    let word = word.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'));
    let token_chars = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.');
    if word.len() < MIN_SECRET_CHARS || !word.chars().all(token_chars) {
        return false;
    }
    if SECRET_PREFIXES.iter().any(|prefix| word.starts_with(prefix)) {
        return true;
    }
    if word.len() < MIN_RANDOM_CHARS {
        return false;
    }
    if !word.contains('-') && word.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    let upper = word.chars().any(|c| c.is_ascii_uppercase());
    let lower = word.chars().any(|c| c.is_ascii_lowercase());
    let digit = word.chars().any(|c| c.is_ascii_digit());
    upper && lower && digit
}

/// `text` with secret-looking words and PEM blocks replaced by `[redacted]`, cut to [`MAX_TEXT_CHARS`].
pub fn scrub(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find("-----BEGIN") {
        let end = text[start..]
            .find("-----END")
            .and_then(|end| text[start + end + 8..].find("-----").map(|close| start + end + 8 + close + 5))
            .unwrap_or(text.len());
        text.replace_range(start..end, REDACTED);
    }
    let mut scrubbed = String::with_capacity(text.len());
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        if looks_like_secret(word) {
            scrubbed.push_str(REDACTED);
            scrubbed.push_str(&piece[word.len()..]);
        } else {
            scrubbed.push_str(piece);
        }
    }
    cut(&scrubbed, MAX_TEXT_CHARS)
}

/// Engine text worth keeping: trimmed, scrubbed and cut; empty text is none.
pub fn clean(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|text| !text.is_empty()).map(scrub)
}

pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_and_personal_columns_are_sensitive() {
        for name in [
            "password",
            "user_password",
            "passwordHash",
            "PASSWD",
            "api_key",
            "apiKey",
            "ApiKey",
            "secret",
            "refresh_token",
            "private_key",
            "salt",
            "pin_hash",
            "ssn",
            "card_number",
            "cardNumber",
            "cvv",
            "iban",
        ] {
            assert!(is_sensitive_name(name), "{name}");
        }
        for name in ["id", "name", "discarded", "cardinality", "hashtag", "customer_id", "email_verified"] {
            assert!(!is_sensitive_name(name), "{name}");
        }
    }

    #[test]
    fn secret_looking_text_is_redacted() {
        assert_eq!(scrub("key sk-abcdefghijklmnopqrstuv1234"), "key [redacted]");
        assert_eq!(scrub("token: 'AKIAIOSFODNN7EXAMPLE'"), "token: [redacted]");
        assert_eq!(
            scrub("hash 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
            "hash [redacted]"
        );
        assert_eq!(
            scrub("cert -----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY----- end"),
            "cert [redacted] end"
        );
        // Ordinary text, identifiers and UUIDs stay.
        for text in [
            "The customer's billing address",
            "a2b3c4d5-e6f7-4890-abcd-ef0123456789",
            "order_status_code",
            "CamelCaseIdentifierWithoutDigits",
        ] {
            assert_eq!(scrub(text), text);
        }
        assert_eq!(scrub(&"x".repeat(600)).chars().count(), MAX_TEXT_CHARS + 1);
        assert_eq!(clean(Some("   ")), None);
    }
}
