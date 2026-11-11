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
const SENSITIVE_PARTS: [&str; 10] = [
    "password",
    "passwd",
    "passphrase",
    "passcode",
    "secret",
    "token",
    "apikey",
    "api_key",
    "private",
    "credential",
];
/// Words a column name has as a whole word (`card_number`, `pin_hash`, `totpSeed`, `session-id`) to count as sensitive.
const SENSITIVE_WORDS: [&str; 25] = [
    "salt",
    "hash",
    "ssn",
    "card",
    "cvv",
    "iban",
    "pwd",
    "pin",
    "otp",
    "totp",
    "hotp",
    "mfa",
    "nonce",
    "cookie",
    "cookies",
    "bearer",
    "session",
    "sessionid",
    "sessid",
    "dob",
    "birth",
    "birthdate",
    "birthday",
    "dateofbirth",
    "jwt",
];
/// Words that make the next word, `key` or `code`, sensitive (`encryption_key`, `aws_access_key_id`, `licenseKey`,
/// `auth-code`), also written as one word (`signingkey`).
const KEY_QUALIFIERS: [&str; 17] = [
    "encryption",
    "decryption",
    "signing",
    "sign",
    "access",
    "license",
    "licence",
    "private",
    "ssh",
    "secret",
    "master",
    "api",
    "hmac",
    "aes",
    "gpg",
    "pgp",
    "session",
];
const KEY_WORDS: [&str; 2] = ["key", "keys"];
const CODE_QUALIFIERS: [&str; 6] = ["auth", "recovery", "backup", "verification", "reset", "mfa"];

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
    if joined.contains("apikey") || words.iter().any(|word| SENSITIVE_WORDS.contains(&word.as_str())) {
        return true;
    }
    let compound = |qualifiers: &[&str], nouns: &[&str], word: &str| {
        qualifiers
            .iter()
            .any(|qualifier| nouns.iter().any(|noun| word.strip_prefix(qualifier) == Some(*noun)))
    };
    let pair = |qualifiers: &[&str], nouns: &[&str], first: &str, second: &str| qualifiers.contains(&first) && nouns.contains(&second);
    words
        .iter()
        .any(|word| compound(&KEY_QUALIFIERS, &KEY_WORDS, word) || compound(&CODE_QUALIFIERS, &["code"], word))
        || words.windows(2).any(|pairs| {
            pair(&KEY_QUALIFIERS, &KEY_WORDS, &pairs[0], &pairs[1]) || pair(&CODE_QUALIFIERS, &["code", "codes"], &pairs[0], &pairs[1])
        })
}

/// Prefixes of well-known credentials (OpenAI and Anthropic, GitHub, GitLab, Slack, AWS, Google, JWT, Stripe).
const SECRET_PREFIXES: [&str; 18] = [
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xox",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "eyJ",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "whsec_",
];
const MIN_SECRET_CHARS: usize = 20;
const MIN_RANDOM_CHARS: usize = 32;
/// Characters that join a secret to other text in one word (`key:sk-…`, `user:pass@host`, `?token=…&`).
const JOINERS: [char; 7] = [':', '@', '?', '&', '=', ',', ';'];
/// Authorization schemes whose next word is a credential.
const AUTH_SCHEMES: [&str; 2] = ["bearer", "basic"];
/// Webhook URLs whose path is the credential.
const WEBHOOK_PATHS: [&str; 5] = [
    "hooks.slack.com/services/",
    "hooks.slack.com/workflows/",
    "hooks.slack.com/triggers/",
    "discord.com/api/webhooks/",
    "discordapp.com/api/webhooks/",
];

fn quote_like(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '<' | '>')
}

fn trim_quotes(word: &str) -> &str {
    word.trim_matches(|c: char| quote_like(c) || matches!(c, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'))
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.')
}

/// Whether one piece of a word looks like a credential: a known key prefix, a long hex string, or a long mix of upper
/// case, lower case and digits (base64 or base62 tokens).
fn looks_like_secret(word: &str) -> bool {
    let word = trim_quotes(word);
    if word.len() < MIN_SECRET_CHARS || !word.chars().all(token_char) {
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

/// The credential after an authorization scheme (`Bearer eyJ…`, `Basic dXNlcjpw…`): token characters only, at least 8 of
/// them, and not a plain lower-case word (`basic plan` stays).
fn looks_like_credential(word: &str) -> bool {
    let word = trim_quotes(word);
    word.len() >= 8 && word.chars().all(token_char) && !word.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c == '.')
}

/// `text` with the user information of URLs replaced: `postgres://app:pw@db/prod` → `postgres://[redacted]@db/prod`.
fn redact_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("://") {
        let (head, tail) = rest.split_at(index + 3);
        out.push_str(head);
        let authority_end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace() || quote_like(c))
            .unwrap_or(tail.len());
        let authority = &tail[..authority_end];
        match authority.rfind('@') {
            Some(at) if at > 0 => {
                out.push_str(REDACTED);
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[authority_end..];
    }
    out.push_str(rest);
    out
}

/// `text` with the credential path of webhook URLs (Slack, Discord) replaced.
fn redact_webhooks(text: &str) -> String {
    let mut text = text.to_owned();
    for marker in WEBHOOK_PATHS {
        let mut from = 0;
        while let Some(found) = text[from..].to_ascii_lowercase().find(marker) {
            let start = from + found + marker.len();
            let end = text[start..]
                .find(|c: char| c.is_whitespace() || quote_like(c) || matches!(c, ')' | ']' | '}'))
                .map_or(text.len(), |end| start + end);
            if end > start {
                text.replace_range(start..end, REDACTED);
                from = start + REDACTED.len();
            } else {
                from = start;
            }
        }
    }
    text
}

/// One whitespace-free word with every secret-looking piece (split at [`JOINERS`]) replaced; also says whether the word
/// ends with an authorization scheme, so that the next word is the credential.
fn scrub_word(word: &str) -> (String, bool) {
    if looks_like_secret(word) {
        return (REDACTED.to_owned(), false);
    }
    let mut out = String::with_capacity(word.len());
    let mut last = "";
    for piece in word.split_inclusive(JOINERS) {
        let (body, joiner) = match piece.char_indices().last() {
            Some((index, c)) if JOINERS.contains(&c) => (&piece[..index], &piece[index..]),
            _ => (piece, ""),
        };
        if looks_like_secret(body) {
            out.push_str(REDACTED);
        } else {
            out.push_str(body);
        }
        out.push_str(joiner);
        if !body.is_empty() {
            last = body;
        }
    }
    let scheme = AUTH_SCHEMES.iter().any(|scheme| trim_quotes(last).eq_ignore_ascii_case(scheme));
    (out, scheme)
}

/// `text` with secret-looking words and pieces of words, the user information of URLs, webhook paths, the credential
/// after `Bearer` or `Basic`, and PEM blocks replaced by `[redacted]`, cut to [`MAX_TEXT_CHARS`].
pub fn scrub(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find("-----BEGIN") {
        let end = text[start..]
            .find("-----END")
            .and_then(|end| text[start + end + 8..].find("-----").map(|close| start + end + 8 + close + 5))
            .unwrap_or(text.len());
        text.replace_range(start..end, REDACTED);
    }
    let text = redact_webhooks(&redact_userinfo(&text));
    let mut scrubbed = String::with_capacity(text.len());
    let mut after_scheme = false;
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let space = &piece[word.len()..];
        if word.is_empty() {
            scrubbed.push_str(piece);
            continue;
        }
        if after_scheme && looks_like_credential(word) {
            scrubbed.push_str(REDACTED);
            scrubbed.push_str(space);
            after_scheme = false;
            continue;
        }
        let (word, scheme) = scrub_word(word);
        scrubbed.push_str(&word);
        scrubbed.push_str(space);
        after_scheme = scheme;
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

// --- Identifiers in names: Redis key segments, document field paths ------------------------------

/// An IPv4 or IPv6 address.
pub fn is_ip(text: &str) -> bool {
    text.parse::<std::net::IpAddr>().is_ok()
}

/// A phone number: 7 to 15 digits with an optional leading `+` and the usual separators, and a `+` or a separator (plain
/// digits are numbers).
pub fn is_phone_like(text: &str) -> bool {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    let body = text.strip_prefix('+').unwrap_or(text);
    (7..=15).contains(&digits)
        && !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || matches!(c, '-' | '.' | ' ' | '(' | ')'))
        && (text.starts_with('+') || body.chars().any(|c| matches!(c, '-' | ' ' | '(' | ')')))
        && !is_date_like(body)
}

fn is_date_like(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..10].iter().enumerate().all(|(index, b)| match index {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

/// Text that reads as a token rather than a name: base64 (`+`, `/` or `=` among token characters), longer than 32
/// characters, 16 or more characters mixing letters and digits, or a secret by [`scrub`]'s rules.
pub fn is_token_like(text: &str) -> bool {
    let letters = text.chars().any(|c| c.is_ascii_alphabetic());
    let digits = text.chars().any(|c| c.is_ascii_digit());
    let base64 = text.len() >= 8
        && text.chars().any(|c| matches!(c, '+' | '/' | '='))
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'));
    base64 || text.chars().count() > 32 || (text.len() >= 16 && letters && digits) || looks_like_secret(text)
}

/// An identifier: a UUID, a number, a hex string of 12 or more characters with a digit (an ObjectId), or 8 or more
/// characters with at least 3 digits among letters (`ord8x2k99`).
fn is_identifier(text: &str) -> bool {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    let letters = text.chars().filter(char::is_ascii_alphabetic).count();
    let uuid = text.len() == 36
        && text.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    uuid || (!text.is_empty() && digits == text.len())
        || (text.len() >= 12 && digits > 0 && text.chars().all(|c| c.is_ascii_hexdigit()))
        || (text.len() >= 8 && digits >= 3 && letters > 0)
}

/// Whether one segment of a field path is a value used as a key (`prefs.ann@example.com`, `balances.5f2b…`,
/// `phones.+15551234567`) rather than a field name.
pub fn is_value_like_segment(segment: &str) -> bool {
    // An at sign past the start (an email); `@timestamp` is a field name.
    segment.find('@').is_some_and(|at| at > 0)
        || segment.contains(char::is_whitespace)
        || segment.chars().count() > 24
        || is_identifier(segment)
        || is_ip(segment)
        || is_phone_like(segment)
        || is_token_like(segment)
}

/// The placeholder of a field path segment that is a value used as a key.
pub const KEY_PLACEHOLDER: &str = "{key}";

/// A document field path with every value-like segment as [`KEY_PLACEHOLDER`] and the others scrubbed:
/// `prefs.ann@example.com.theme` → `prefs.{key}.theme`.
pub fn field_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('.').collect();
    let mut out: Vec<String> = Vec::with_capacity(segments.len());
    let mut index = 0;
    while index < segments.len() {
        let segment = segments[index];
        index += 1;
        if !is_value_like_segment(segment) {
            out.push(scrub(segment));
            continue;
        }
        // The dots of an email or an address split one key into several segments: `ann@mail.example.com`.
        if segment.find('@').is_some_and(|at| at > 0) {
            let ahead = segments.len().min(index + EMAIL_DOMAIN_LABELS);
            if let Some(tld) = (index..ahead)
                .rev()
                .find(|&next| TOP_LEVEL_DOMAINS.contains(&segments[next].to_ascii_lowercase().as_str()))
            {
                index = tld + 1;
            }
        }
        // Several value-like segments in a row (`203.0.113.5`) are one key.
        if out.last().map(String::as_str) != Some(KEY_PLACEHOLDER) {
            out.push(KEY_PLACEHOLDER.to_owned());
        }
    }
    out.join(".")
}

/// Labels of an email's domain looked at after the `@` segment, and the top-level domains that end it.
const EMAIL_DOMAIN_LABELS: usize = 3;
const TOP_LEVEL_DOMAINS: [&str; 24] = [
    "com", "org", "net", "io", "co", "uk", "de", "fr", "edu", "gov", "app", "dev", "ai", "info", "biz", "vn", "jp", "cn", "ru", "br", "au",
    "ca", "us", "me",
];

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

    #[test]
    fn credential_and_personal_column_variants_are_sensitive() {
        for name in [
            "encryption_key",
            "encryptionKey",
            "EncryptionKey",
            "encryption-key",
            "encryptionkey",
            "signing_key",
            "signingKey",
            "aws_access_key_id",
            "awsAccessKeyId",
            "AWS_ACCESS_KEY_ID",
            "access_key",
            "license_key",
            "licenseKey",
            "ssh_key",
            "hmac_key",
            "pin",
            "PIN",
            "pin_code",
            "pinCode",
            "otp",
            "otp_code",
            "totp",
            "totp_seed",
            "totpSeed",
            "mfa",
            "mfa_code",
            "MFACode",
            "passcode",
            "userPasscode",
            "passphrase",
            "session",
            "session_id",
            "sessionId",
            "session-id",
            "sessionid",
            "nonce",
            "cookie",
            "auth_cookies",
            "bearer",
            "bearer_value",
            "auth_code",
            "authCode",
            "recovery_codes",
            "dob",
            "DOB",
            "birth",
            "birth_date",
            "birthDate",
            "birthdate",
            "date_of_birth",
            "dateOfBirth",
            "birthday",
            "jwt",
        ] {
            assert!(is_sensitive_name(name), "{name}");
        }
        for name in [
            "pinned",
            "is_pinned",
            "spin",
            "spinner",
            "options",
            "option_id",
            "topping",
            "keyboard",
            "monkey",
            "keys",
            "primary_key",
            "sort_key",
            "partition_key",
            "cache_key",
            "foreign_keys",
            "access_level",
            "accessed_at",
            "license_type",
            "signing_date",
            "dobby",
            "birthplace",
            "cookbook",
            "code",
            "country_code",
            "zip_code",
            "status_code",
        ] {
            assert!(!is_sensitive_name(name), "{name}");
        }
    }

    #[test]
    fn secrets_joined_to_other_text_and_in_urls_are_redacted() {
        assert_eq!(scrub("postgres://app:Hunter2Secret@db/prod"), "postgres://[redacted]@db/prod");
        assert_eq!(
            scrub("see mongodb+srv://admin:pw@cluster0.example.net/?retryWrites=true now"),
            "see mongodb+srv://[redacted]@cluster0.example.net/?retryWrites=true now"
        );
        assert_eq!(
            scrub("https://example.com/a@b"),
            "https://example.com/a@b",
            "an @ in the path is no user"
        );
        assert_eq!(
            scrub("posts to https://hooks.slack.com/services/T000/B000/XXXXXXXX daily"),
            "posts to https://hooks.slack.com/services/[redacted] daily"
        );
        assert_eq!(
            scrub("(https://discord.com/api/webhooks/123/abc-DEF)"),
            "(https://discord.com/api/webhooks/[redacted])"
        );
        assert_eq!(scrub("key:sk-proj-abcdefghijklmnopqrst"), "key:[redacted]");
        assert_eq!(
            scrub("Authorization:Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc"),
            "Authorization:Bearer [redacted]"
        );
        assert_eq!(scrub("Authorization: Bearer abcdef0123456789"), "Authorization: Bearer [redacted]");
        assert_eq!(scrub("header Basic dXNlcjpwYXNzd29yZA=="), "header Basic [redacted]");
        assert_eq!(scrub("a basic plan for everyone"), "a basic plan for everyone");
        assert_eq!(
            scrub("url?token=ghp_abcdefghijklmnopqrstuvwxyz0123&x=1"),
            "url?token=[redacted]&x=1"
        );
        assert_eq!(scrub("slack=xoxb-123456789012-abcdefghijkl;"), "slack=[redacted];");
        assert_eq!(scrub("aws AKIAIOSFODNN7EXAMPLE,next"), "aws [redacted],next");
        // Ordinary joined text stays.
        for text in [
            "status:paid,new",
            "a=b&c=d",
            "user@example.com",
            "ratio 3:2",
            "http://localhost:8080/path",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn value_like_path_segments_collapse() {
        assert_eq!(field_path("prefs.ann@example.com.theme"), "prefs.{key}.theme");
        assert_eq!(field_path("balances.5f2b9c0e1d2a3b4c5d6e7f80"), "balances.{key}");
        assert_eq!(field_path("byUser.a2b3c4d5-e6f7-4890-abcd-ef0123456789"), "byUser.{key}");
        assert_eq!(field_path("phones.+15551234567"), "phones.{key}");
        assert_eq!(field_path("hits.203.0.113.5"), "hits.{key}");
        assert_eq!(field_path("prefs.ann@mail.example.co.uk.theme"), "prefs.{key}.theme");
        assert_eq!(field_path("apiKeys.Zm9vYmFyYmF6cXV4"), "apiKeys.{key}");
        assert_eq!(field_path("notes.a very long sentence used as a key"), "notes.{key}");
        assert_eq!(field_path("orders.42.total"), "orders.{key}.total");
        for path in [
            "_id",
            "contact.email",
            "items.sku",
            "address2.line1",
            "@timestamp",
            "title.keyword",
            "user_id",
        ] {
            assert_eq!(field_path(path), path);
        }
        assert!(is_ip("::1") && is_ip("203.0.113.5") && !is_ip("1.2.3"));
        assert!(is_phone_like("+15551234567") && is_phone_like("(555) 123-4567"));
        assert!(!is_phone_like("2026-11-12") && !is_phone_like("12345678"));
        assert!(is_token_like("Zm9vYmFy+cXV4==") && is_token_like("a1b2c3d4e5f6g7h8"));
        assert!(!is_token_like("profile") && !is_token_like("notifications_queue"));
    }
}
