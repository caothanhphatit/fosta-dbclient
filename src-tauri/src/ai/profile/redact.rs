//! What a profile never keeps: system and credential objects (left out of the listing), the comments and defaults of
//! columns whose names look like credentials or personal data, and secret-looking text anywhere (API keys, tokens,
//! private keys). Engine-neutral: system containers are recognised by the `system` note engines put on their tree nodes
//! (MySQL `mysql`, `sys`…; MongoDB `admin`, `config`, `local`; SQL Server `master`…; ClickHouse `system`), plus names
//! every engine family shares.
//!
//! The same rules redact the rows and documents the assistant's read tools (`run_read_query`, `sample_documents`) hand
//! to the model and show in the chat ([`redact_cell`], [`redact_json`]): fields named like credentials keep their key
//! and lose their value, and secret-looking text is replaced, at any depth. Columns named like personal data
//! ([`is_personal_name`]) are never sampled while learning.

use serde_json::Value as Json;

use crate::contract::{NodeKind, ObjectNode, Value};

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
/// What replaces a redacted value or piece of text.
pub const REDACTED: &str = "[redacted]";

/// Words a column name contains to count as sensitive, compared lower-cased (Unicode): English, and the usual words for
/// "password" in other languages (German `passwort` / `kennwort`, Spanish `contraseña`, Dutch `wachtwoord`, Russian
/// `пароль`).
const SENSITIVE_PARTS: [&str; 16] = [
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
    "passwort",
    "kennwort",
    "contrasena",
    "contraseña",
    "wachtwoord",
    "пароль",
];
/// Words a column name has as a whole word (`card_number`, `pin_hash`, `totpSeed`, `session-id`) to count as sensitive.
const SENSITIVE_WORDS: [&str; 31] = [
    "pass",
    "pw",
    "pword",
    "senha",
    "parola",
    "matkhau",
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
/// Word sequences that name a password in other languages (French `mot_de_passe`, Vietnamese `mat_khau`), also
/// written as one word (`motdepasse`, `matkhau`).
const SENSITIVE_PHRASES: [&[&str]; 3] = [&["mot", "de", "passe"], &["mat", "khau"], &["mật", "khẩu"]];
/// Words that make the next word, `key` or `code`, sensitive (`encryption_key`, `aws_access_key_id`, `licenseKey`,
/// `auth-code`), also written as one word (`signingkey`).
const KEY_QUALIFIERS: [&str; 21] = [
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
    "activation",
    "reset",
    "invite",
    "confirmation",
];
const KEY_WORDS: [&str; 2] = ["key", "keys"];
const CODE_QUALIFIERS: [&str; 9] = [
    "auth",
    "recovery",
    "backup",
    "verification",
    "reset",
    "mfa",
    "activation",
    "invite",
    "confirmation",
];

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
    let lower = name.to_lowercase();
    if SENSITIVE_PARTS.iter().any(|part| lower.contains(part)) {
        return true;
    }
    let words = words(name);
    let joined = words.concat();
    let phrase = |phrase: &&[&str]| {
        joined == phrase.concat()
            || words
                .windows(phrase.len())
                .any(|window| window.iter().zip(phrase.iter()).all(|(a, b)| a == b))
    };
    if joined.contains("apikey")
        || words.iter().any(|word| SENSITIVE_WORDS.contains(&word.as_str()))
        || SENSITIVE_PHRASES.iter().any(phrase)
    {
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

/// Whole words of a column name that make it personal data: contact details, addresses, network addresses, person
/// names, identity documents.
const PERSONAL_WORDS: [&str; 30] = [
    "email",
    "emails",
    "emailaddress",
    "phone",
    "phones",
    "phonenumber",
    "telephone",
    "msisdn",
    "fax",
    "address",
    "addresses",
    "street",
    "ip",
    "ipaddr",
    "ipaddress",
    "ipv4",
    "ipv6",
    "macaddr",
    "macaddress",
    "surname",
    "fullname",
    "firstname",
    "lastname",
    "middlename",
    "givenname",
    "familyname",
    "displayname",
    "username",
    "nickname",
    "passport",
];
/// Words that make a column personal data when they end its name (`login`, `user`, `created_by_user`, `owner`,
/// `contact`, `mobile`): such columns hold who someone is or how to reach them. `user_type` or `owner_kind` still say
/// what kind of thing a row is, so they stay.
const PERSON_NOUNS: [&str; 22] = [
    "login",
    "logins",
    "user",
    "users",
    "userid",
    "author",
    "authors",
    "owner",
    "owners",
    "creator",
    "assignee",
    "reporter",
    "reviewer",
    "approver",
    "contact",
    "mobile",
    "tel",
    "cellphone",
    "mail",
    "vat",
    "nin",
    "postcode",
];
/// Words that make the next word, `id`, `no`, `number` or `code`, an identity or account identifier (`tax_id`,
/// `national_id`, `account_number`, `social_security_no`).
const ID_QUALIFIERS: [&str; 10] = [
    "tax",
    "national",
    "account",
    "acct",
    "vat",
    "security",
    "insurance",
    "driver",
    "licence",
    "license",
];
/// Words that make the next word, `by`, name a person (`created_by`, `updatedBy`, `assigned_by`).
const ACTOR_VERBS: [&str; 10] = [
    "created",
    "updated",
    "modified",
    "deleted",
    "changed",
    "edited",
    "assigned",
    "approved",
    "reviewed",
    "submitted",
];
/// Words that make the next word, `name`, a person's name (`first_name`, `customerName`, `contact.name`).
const NAME_QUALIFIERS: [&str; 22] = [
    "full",
    "first",
    "last",
    "middle",
    "given",
    "family",
    "display",
    "user",
    "nick",
    "maiden",
    "legal",
    "contact",
    "customer",
    "person",
    "owner",
    "sender",
    "recipient",
    "employee",
    "member",
    "holder",
    "patient",
    "client",
];

/// Whether a column or field path called `name` holds personal data (`email`, `contact.phone`, `billing_address`,
/// `first_name`, `customerName`, `ip_address`, `mac_addr`). Such columns are never sampled while learning; the read tools
/// still return them where the user lets the assistant read data, since answering about the data needs them.
pub fn is_personal_name(name: &str) -> bool {
    let words = words(name);
    words.iter().any(|word| PERSONAL_WORDS.contains(&word.as_str()))
        || words.last().is_some_and(|last| PERSON_NOUNS.contains(&last.as_str()))
        || words.windows(2).any(|pair| {
            let (first, second) = (pair[0].as_str(), pair[1].as_str());
            (first == "e" && second == "mail")
                || (first == "mac" && matches!(second, "address" | "addr"))
                || (first == "mobile" && matches!(second, "number" | "phone" | "no"))
                || (NAME_QUALIFIERS.contains(&first) && matches!(second, "name" | "names"))
                || (ID_QUALIFIERS.contains(&first) && matches!(second, "id" | "no" | "number" | "num" | "code"))
                || (ACTOR_VERBS.contains(&first) && second == "by")
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

/// [`trim_quotes`], and the `…` a cut value ends with (the collection workspace cuts long values before they reach
/// the backend), so a cut secret is still recognised.
fn trim_for_check(word: &str) -> &str {
    let mut word = trim_quotes(word);
    while let Some(rest) = word.strip_suffix('…') {
        word = trim_quotes(rest);
    }
    word
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.')
}

/// Whether one piece of a word looks like a credential: a known key prefix, a long hex string, or a long mix of upper
/// case, lower case and digits (base64 or base62 tokens).
fn looks_like_secret(word: &str) -> bool {
    let word = trim_for_check(word);
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

/// Modular-crypt, framework and LDAP password hashes (`$2b$12$…`, `$argon2id$v=19$…`, `pbkdf2_sha256$…`, WordPress
/// `$P$…` / `$wp$2y$…`, Drupal `$S$…`, phpBB `$H$…`, Apache `$apr1$…`, Spring `{bcrypt}$2a$…`, LDAP `{SSHA}…`), which
/// [`looks_like_secret`] misses for their `$` and braces. Compared case-insensitively.
const HASH_PREFIXES: [&str; 35] = [
    "$2a$",
    "$2b$",
    "$2x$",
    "$2y$",
    "$1$",
    "$3$",
    "$5$",
    "$6$",
    "$7$",
    "$y$",
    "$gy$",
    "$p$",
    "$h$",
    "$s$",
    "$wp$",
    "$apr1$",
    "$md5",
    "$sha1$",
    "$sha256$",
    "$sha512$",
    "$argon2",
    "$scrypt$",
    "$pbkdf2",
    "pbkdf2_sha",
    "bcrypt$",
    "bcrypt_sha256$",
    "argon2$",
    "{ssha",
    "{sha",
    "{smd5}",
    "{md5}",
    "{crypt}",
    "{bcrypt}",
    "{pbkdf2",
    "{argon2",
];
/// Spring Security prefixes whose password follows in plain text or in any length.
const PLAIN_PREFIXES: [&str; 3] = ["{noop}", "{plain}", "{cleartext}"];
const MIN_HASH_CHARS: usize = 20;

fn looks_like_password_hash(word: &str) -> bool {
    // Braces start LDAP and Spring prefixes, so only quotes and punctuation are trimmed.
    let word = word
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')'))
        .trim_end_matches('…');
    let starts = |prefix: &&str| word.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix));
    if PLAIN_PREFIXES.iter().any(|prefix| starts(prefix) && word.len() > prefix.len()) {
        return true;
    }
    if word.len() < MIN_HASH_CHARS {
        return false;
    }
    if HASH_PREFIXES.iter().any(starts) || looks_like_modular_crypt(word) {
        return true;
    }
    // A Spring `{id}` prefix of any name (`{scrypt}`, `{pbkdf2@SpringSecurity_v5_8}`) before a hash.
    word.strip_prefix('{')
        .and_then(|rest| rest.split_once('}'))
        .is_some_and(|(id, hash)| {
            (2..=40).contains(&id.len())
                && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '@' | '.'))
                && hash.len() >= 16
                && (looks_like_secret(hash)
                    || looks_like_modular_crypt(hash)
                    || HASH_PREFIXES.iter().any(|prefix| hash.starts_with(prefix)))
        })
}

/// The generic modular-crypt shape `$id$…$…` (`$6$rounds=5000$salt$hash`): a short scheme id between the first two
/// `$`, then at least 16 characters of salt and hash with another `$` among them. A price (`$25`) has no second `$`.
fn looks_like_modular_crypt(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('$') else {
        return false;
    };
    let Some((id, tail)) = rest.split_once('$') else {
        return false;
    };
    (1..=12).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && tail.len() >= 16
        && tail.contains('$')
        && tail.chars().all(|c| token_char(c) || matches!(c, '$' | ','))
}

/// The credential after an authorization scheme (`Bearer eyJ…`, `Basic dXNlcjpw…`): token characters only, at least 8 of
/// them, and not a plain lower-case word (`basic plan` stays).
fn looks_like_credential(word: &str) -> bool {
    let word = trim_for_check(word);
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

/// What the word after a scrubbed word is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    Plain,
    /// The word ended with an authorization scheme (`Bearer`): the next word is a credential when it looks like one.
    Credential,
    /// The word was a sensitive key with its joiner (`password:`, `DB_PASSWORD=`): the next word is its value.
    Value,
}

/// Joiners after which a sensitive key's value starts (`password=…`, `"token":"…"`).
const VALUE_JOINERS: [char; 2] = ['=', ':'];
/// Joiners that end a sensitive key's value (`Password=x;User=y`, `?password=x&user=y`, `"password":"x","a":1`).
const VALUE_ENDS: [char; 3] = [';', '&', ','];

/// `body` with its text replaced and its surrounding quotes and brackets kept (`"hunter2"}` → `"[redacted]"}`).
fn redact_core(body: &str) -> String {
    let core = trim_quotes(body);
    if core.is_empty() {
        return body.to_owned();
    }
    let start = body.find(core).unwrap_or(0);
    format!("{}{REDACTED}{}", &body[..start], &body[start + core.len()..])
}

/// A pattern placeholder: `{id}`, `{key}`, `{token}`.
fn is_placeholder(body: &str) -> bool {
    body.strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|inner| !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

/// Whether `body` (`password`, `"DB_PASSWORD"`, `--password`, `?api_key`) names a credential.
fn is_sensitive_key(body: &str) -> bool {
    let key = trim_quotes(body).trim_start_matches(['-', '?', '&', '$']);
    !key.is_empty() && key.chars().any(char::is_alphabetic) && is_sensitive_name(key)
}

/// One whitespace-free word with every secret-looking piece (split at [`JOINERS`]) replaced, the value after a
/// sensitive key (`password=…`, `"token":"…"`, `?password=…&`) replaced up to the next `;`, `&` or `,`, and a password
/// hash after a joiner (`1700000000:$P$B…`) replaced with the rest of the word; also says what the next word is.
fn scrub_word(word: &str) -> (String, Next) {
    if looks_like_secret(word) || looks_like_password_hash(word) || dollar_secret(word) {
        return (REDACTED.to_owned(), Next::Plain);
    }
    let mut out = String::with_capacity(word.len());
    let mut last = "";
    let mut in_value = false;
    let mut offset = 0;
    let mut next = Next::Plain;
    for piece in word.split_inclusive(JOINERS) {
        let rest = &word[offset..];
        offset += piece.len();
        let (body, joiner) = match piece.char_indices().last() {
            Some((index, c)) if JOINERS.contains(&c) => (&piece[..index], &piece[index..]),
            _ => (piece, ""),
        };
        if !in_value && offset > piece.len() && (looks_like_password_hash(rest) || dollar_secret(rest)) {
            out.push_str(REDACTED);
            return (out, Next::Plain);
        }
        let ends_value = joiner.chars().next().is_some_and(|c| VALUE_ENDS.contains(&c));
        if in_value {
            // A placeholder (`session:{id}`, a key pattern) holds no value.
            let placeholder = is_placeholder(body);
            if placeholder {
                out.push_str(body);
            } else {
                out.push_str(&redact_core(body));
            }
            out.push_str(joiner);
            in_value = !placeholder && !ends_value && !joiner.is_empty();
            continue;
        }
        if looks_like_secret(body) {
            out.push_str(REDACTED);
        } else {
            out.push_str(body);
        }
        out.push_str(joiner);
        if joiner.chars().next().is_some_and(|c| VALUE_JOINERS.contains(&c)) && is_sensitive_key(body) {
            in_value = true;
            // The joiner ends the word (`password: hunter2`): the value is the next word.
            next = if offset == word.len() { Next::Value } else { Next::Plain };
        }
        if !body.is_empty() {
            last = body;
        }
    }
    if next == Next::Plain && !in_value && AUTH_SCHEMES.iter().any(|scheme| trim_quotes(last).eq_ignore_ascii_case(scheme)) {
        next = Next::Credential;
    }
    (out, next)
}

/// A word with `$` (which [`looks_like_secret`] does not allow) one of whose `$`-separated pieces is a secret, so a
/// `$` inside or around a token does not hide it.
fn dollar_secret(word: &str) -> bool {
    word.contains('$') && word.split('$').any(looks_like_secret)
}

/// `text` with secret-looking words and pieces of words, the user information of URLs, webhook paths, the credential
/// after `Bearer` or `Basic`, and PEM blocks replaced by `[redacted]`, cut to [`MAX_TEXT_CHARS`].
pub fn scrub(text: &str) -> String {
    cut(&scrub_uncut(text), MAX_TEXT_CHARS)
}

/// [`scrub`] without the cut.
fn scrub_uncut(text: &str) -> String {
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
    let mut next = Next::Plain;
    let mut previous_key = false;
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let space = &piece[word.len()..];
        if word.is_empty() {
            scrubbed.push_str(piece);
            continue;
        }
        // `DB_PASSWORD = hunter2`: a lone `=` or `:` after a sensitive key.
        if previous_key && (word == "=" || word == ":") {
            scrubbed.push_str(piece);
            next = Next::Value;
            previous_key = false;
            continue;
        }
        previous_key = is_sensitive_key(word) && !word.contains(JOINERS);
        match next {
            Next::Value => {
                scrubbed.push_str(&redact_core(word));
                scrubbed.push_str(space);
                next = Next::Plain;
                previous_key = false;
                continue;
            }
            Next::Credential if looks_like_credential(word) => {
                scrubbed.push_str(REDACTED);
                scrubbed.push_str(space);
                next = Next::Plain;
                continue;
            }
            _ => {}
        }
        let (word, after) = scrub_word(word);
        scrubbed.push_str(&word);
        scrubbed.push_str(space);
        next = after;
    }
    scrubbed
}

/// [`scrub`] without the cut, for text the caller has already bounded (error messages, task logs).
pub fn scrub_text(text: &str) -> String {
    scrub_uncut(text)
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

// --- Rows and documents the assistant reads -------------------------------------------------------

/// Text longer than this is cut before it is scrubbed (the read tools show far less of it).
const MAX_SCRUBBED_CHARS: usize = 4_096;
/// Nesting redacted; anything deeper is replaced as a whole.
const MAX_REDACT_DEPTH: usize = 100;
/// JSON text longer than this inside a value is scrubbed as text rather than parsed.
const MAX_NESTED_JSON_CHARS: usize = 256 * 1024;

/// One value's text with secret-looking pieces replaced, at most [`MAX_SCRUBBED_CHARS`] of it.
pub fn scrub_value(text: &str) -> String {
    if text.len() <= MAX_SCRUBBED_CHARS {
        return scrub_uncut(text);
    }
    scrub_uncut(&cut(text, MAX_SCRUBBED_CHARS))
}

/// A JSON document (or array) inside text, when the text is one.
fn json_in(text: &str) -> Option<Json> {
    let trimmed = text.trim();
    let bracketed = (trimmed.starts_with('{') && trimmed.ends_with('}')) || (trimmed.starts_with('[') && trimmed.ends_with(']'));
    bracketed.then(|| serde_json::from_str::<Json>(trimmed).ok()).flatten()
}

/// A JSON value as the assistant may see it: the value of every field named like a credential
/// ([`is_sensitive_name`]) is `[redacted]` (its key stays; a null stays null), and secret-looking text in strings and
/// keys is replaced, at any depth and inside arrays.
pub fn redact_json(value: &Json) -> Json {
    redact_json_at(value, 0)
}

fn redact_json_at(value: &Json, depth: usize) -> Json {
    if depth > MAX_REDACT_DEPTH {
        return Json::String(REDACTED.to_owned());
    }
    match value {
        // JSON kept as a string (a logged request body) is parsed and redacted like the rest.
        Json::String(text) => Json::String(redact_text_at(text, depth + 1)),
        Json::Array(items) => Json::Array(items.iter().map(|item| redact_json_at(item, depth + 1)).collect()),
        Json::Object(map) => Json::Object(
            map.iter()
                .map(|(key, inner)| {
                    let inner = if is_sensitive_name(key) && !inner.is_null() {
                        Json::String(REDACTED.to_owned())
                    } else {
                        redact_json_at(inner, depth + 1)
                    };
                    (scrub_value(key), inner)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// One cell of a result row as the assistant may see it: `[redacted]` when its column is named like a credential (a
/// null stays null), otherwise [`redact_json`]'s rules applied to nested documents, arrays, JSON and text.
pub fn redact_cell(column: &str, value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        _ if is_sensitive_name(column) => Value::Text(REDACTED.to_owned()),
        other => redact_value(other, 0),
    }
}

fn redact_value(value: &Value, depth: usize) -> Value {
    if depth > MAX_REDACT_DEPTH {
        return Value::Text(REDACTED.to_owned());
    }
    match value {
        Value::Text(text) => Value::Text(redact_text_at(text, depth)),
        Value::Json(text) => Value::Json(redact_text_at(text, depth)),
        Value::Other { type_name, text } => Value::Other {
            type_name: type_name.clone(),
            text: scrub_value(text),
        },
        Value::Array(items) => Value::Array(items.iter().map(|item| redact_value(item, depth + 1)).collect()),
        Value::Document(fields) => Value::Document(
            fields
                .iter()
                .map(|(key, inner)| {
                    let inner = match inner {
                        Value::Null => Value::Null,
                        _ if is_sensitive_name(key) => Value::Text(REDACTED.to_owned()),
                        _ => redact_value(inner, depth + 1),
                    };
                    (scrub_value(key), inner)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Text that is a JSON document redacted as one (re-serialised only when something changed), other text scrubbed.
/// JSON inside JSON strings is parsed too, up to [`MAX_REDACT_DEPTH`] levels in all and [`MAX_NESTED_JSON_CHARS`] per
/// string; longer text is only scrubbed.
fn redact_text_at(text: &str, depth: usize) -> String {
    let parsed = if depth == 0 || text.len() <= MAX_NESTED_JSON_CHARS {
        json_in(text)
    } else {
        None
    };
    match parsed {
        Some(json) => {
            let redacted = redact_json_at(&json, depth);
            if redacted == json {
                text.to_owned()
            } else {
                redacted.to_string()
            }
        }
        None => scrub_value(text),
    }
}

/// Documents given as text (one JSON value, or one per line, as the collection workspace sends them with the chat
/// context) with [`redact_json`]'s rules applied; lines that are not JSON are scrubbed.
pub fn redact_documents_text(text: &str) -> String {
    if let Some(json) = json_in(text) {
        return redact_json(&json).to_string();
    }
    text.lines()
        .map(|line| match json_in(line) {
            Some(json) => redact_json(&json).to_string(),
            None => scrub_uncut(line),
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Text that reads as a token rather than a name: base64 (`+` or `=` among token characters, or `/` among upper case,
/// lower case and digits; `user/alice` is a path), longer than 32 characters, 16 or more characters mixing letters and
/// digits, or a secret by [`scrub`]'s rules.
pub fn is_token_like(text: &str) -> bool {
    let letters = text.chars().any(|c| c.is_ascii_alphabetic());
    let digits = text.chars().any(|c| c.is_ascii_digit());
    is_base64_like(text) || text.chars().count() > 32 || (text.len() >= 16 && letters && digits) || looks_like_secret(text)
}

/// Base64 text: token characters with `+` or `=`, or with `/` among upper case, lower case and digits (`user/alice` is
/// a path).
pub fn is_base64_like(text: &str) -> bool {
    let digits = text.chars().any(|c| c.is_ascii_digit());
    let mixed = text.chars().any(|c| c.is_ascii_uppercase()) && text.chars().any(|c| c.is_ascii_lowercase()) && digits;
    text.len() >= 8
        && (text.chars().any(|c| matches!(c, '+' | '=')) || (text.contains('/') && mixed))
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
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

/// Separators of the segments of a key pattern (`user:{id}:profile`, `cache/{name}`).
const KEY_SEPARATORS: [char; 3] = [':', '/', '|'];

/// A key pattern an engine reported, with every segment that is not a placeholder checked again: emails, IP addresses,
/// phone numbers, identifiers and token-like text become placeholders, the rest is scrubbed. An address with `:` (IPv6)
/// is expected as the engine's `{ip}`.
pub fn key_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for piece in pattern.split_inclusive(KEY_SEPARATORS) {
        let (segment, separator) = match piece.char_indices().last() {
            Some((at, c)) if KEY_SEPARATORS.contains(&c) => (&piece[..at], &piece[at..]),
            _ => (piece, ""),
        };
        let placeholder = segment.starts_with('{') && segment.ends_with('}') && !segment.contains(|c: char| c.is_whitespace());
        out.push_str(&if placeholder || segment.is_empty() || segment == "…" {
            segment.to_owned()
        } else if segment.find('@').is_some_and(|at| at > 0) {
            "{email}".to_owned()
        } else if is_ip(segment) {
            "{ip}".to_owned()
        } else if is_phone_like(segment) {
            "{phone}".to_owned()
        } else if is_identifier(segment) {
            "{id}".to_owned()
        } else if is_token_like(segment) {
            "{token}".to_owned()
        } else {
            scrub(segment)
        });
        out.push_str(separator);
    }
    out
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
        assert_eq!(scrub("token: 'AKIAIOSFODNN7EXAMPLE'"), "token: '[redacted]'");
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
    fn personal_data_columns_are_recognised() {
        for name in [
            "email",
            "Email",
            "e_mail",
            "contact.email",
            "emailAddress",
            "phone",
            "contact.phone",
            "phone_number",
            "mobile_number",
            "address",
            "billing_address",
            "street",
            "ip",
            "ip_address",
            "client_ip",
            "IPAddress",
            "mac_address",
            "macAddr",
            "full_name",
            "first_name",
            "lastName",
            "customerName",
            "contact.name",
            "surname",
            "username",
            "passport_no",
        ] {
            assert!(is_personal_name(name), "{name}");
        }
        for name in [
            "status",
            "name",
            "product_name",
            "category_name",
            "file_name",
            "country",
            "zip",
            "tip",
            "mailbox_size",
        ] {
            assert!(!is_personal_name(name), "{name}");
        }
    }

    #[test]
    fn password_hashes_are_redacted() {
        assert_eq!(scrub("$2b$12$KIXQJFakeHashValueForTests0123456789abcdefABCDEF"), REDACTED);
        assert_eq!(
            scrub("hash: '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'"),
            format!("hash: '{REDACTED}'")
        );
        assert_eq!(scrub("{SSHA}W6ph5Mm5Pz8GgiULbPgzG37mj9g="), REDACTED);
        assert_eq!(scrub("costs $25 today"), "costs $25 today");
    }

    #[test]
    fn nested_documents_lose_credential_values_and_secrets() {
        let document = serde_json::json!({
            "_id": { "$oid": "5f2b9c0e1d2a3b4c5d6e7f80" },
            "email": "ann@example.com",
            "contact": { "email": "ann@example.com", "password": "secret-0", "apiKey": null },
            "logins": [
                { "at": "2026-11-12", "sessionToken": "abc" },
                { "note": "used key sk-abcdefghijklmnopqrstuvwx1234" }
            ],
            "credentials": { "user": "ann", "pass": "pw" },
            "contact.password": "flat-0",
            "tags": ["vip", "$2b$12$KIXQJFakeHashValueForTests0123456789abcdefABCDEF"]
        });
        let redacted = redact_json(&document);
        assert_eq!(redacted["_id"]["$oid"], "5f2b9c0e1d2a3b4c5d6e7f80");
        assert_eq!(redacted["email"], "ann@example.com", "contact data is kept");
        assert_eq!(redacted["contact"]["email"], "ann@example.com");
        assert_eq!(redacted["contact"]["password"], REDACTED);
        assert!(redacted["contact"]["apiKey"].is_null(), "a null stays null");
        assert_eq!(redacted["logins"][0]["sessionToken"], REDACTED);
        assert_eq!(redacted["logins"][0]["at"], "2026-11-12");
        assert_eq!(redacted["logins"][1]["note"], format!("used key {REDACTED}"));
        assert_eq!(redacted["credentials"], REDACTED, "a sensitive field loses its whole subdocument");
        assert_eq!(redacted["contact.password"], REDACTED, "dotted keys are checked too");
        assert_eq!(redacted["tags"], serde_json::json!(["vip", REDACTED]));
        let text = redacted.to_string();
        assert!(
            !text.contains("secret-0") && !text.contains("flat-0") && !text.contains("sk-abc"),
            "{text}"
        );
    }

    #[test]
    fn documents_given_as_text_are_redacted() {
        let lines = "{\"email\": \"ann@example.com\", \"password\": \"secret-0\"}\nnot json key=sk-abcdefghijklmnopqrstuvwx1234";
        let redacted = redact_documents_text(lines);
        assert!(!redacted.contains("secret-0") && !redacted.contains("sk-abc"), "{redacted}");
        assert!(
            redacted.contains("ann@example.com") && redacted.contains("\"password\":\"[redacted]\""),
            "{redacted}"
        );
        let array = redact_documents_text("[{\"contact\": {\"pin\": \"1234\"}}]");
        assert_eq!(
            serde_json::from_str::<Json>(&array).unwrap(),
            serde_json::json!([{ "contact": { "pin": REDACTED } }])
        );
    }

    #[test]
    fn result_cells_lose_credential_values_and_secrets() {
        let text = |value: &str| Value::Text(value.to_owned());
        assert_eq!(redact_cell("password", &text("hunter2")), text(REDACTED));
        assert_eq!(redact_cell("user_password", &Value::Int(4)), text(REDACTED));
        assert_eq!(redact_cell("api_key", &Value::Null), Value::Null);
        assert_eq!(redact_cell("email", &text("ann@example.com")), text("ann@example.com"));
        assert_eq!(
            redact_cell("note", &text("token sk-abcdefghijklmnopqrstuvwx1234")),
            text(&format!("token {REDACTED}"))
        );
        assert_eq!(redact_cell("total", &Value::Int(42)), Value::Int(42));
        // JSON columns and JSON text are redacted inside; untouched JSON keeps its formatting.
        let Value::Json(profile) = redact_cell(
            "profile",
            &Value::Json(r#"{"name": "Ann", "secret": "x", "nested": [{"pin": 1234}]}"#.into()),
        ) else {
            panic!("JSON stays JSON");
        };
        assert_eq!(
            serde_json::from_str::<Json>(&profile).unwrap(),
            serde_json::json!({ "name": "Ann", "secret": REDACTED, "nested": [{ "pin": REDACTED }] })
        );
        assert_eq!(redact_cell("payload", &text(r#"{"a": 1}"#)), text(r#"{"a": 1}"#));
        assert_eq!(
            redact_cell("payload", &text(r#"[{"token": "abc"}]"#)),
            text(r#"[{"token":"[redacted]"}]"#)
        );
        // Nested documents and arrays (MongoDB, Elasticsearch).
        let document = Value::Document(vec![
            ("name".into(), text("Ann")),
            (
                "contact".into(),
                Value::Document(vec![
                    ("email".into(), text("ann@example.com")),
                    ("password".into(), text("secret-1")),
                ]),
            ),
            (
                "keys".into(),
                Value::Array(vec![Value::Document(vec![
                    ("secret".into(), text("s")),
                    ("label".into(), text("ci")),
                ])]),
            ),
        ]);
        let Value::Document(fields) = redact_cell("doc", &document) else {
            panic!("a document stays a document");
        };
        assert_eq!(fields[0], ("name".into(), text("Ann")));
        let Value::Document(contact) = &fields[1].1 else { panic!() };
        assert_eq!(contact[1], ("password".into(), text(REDACTED)));
        assert_eq!(contact[0], ("email".into(), text("ann@example.com")));
        let Value::Array(keys) = &fields[2].1 else { panic!() };
        assert_eq!(
            keys[0],
            Value::Document(vec![("secret".into(), text(REDACTED)), ("label".into(), text("ci"))])
        );
    }

    #[test]
    fn framework_password_columns_and_hashes_are_redacted() {
        for name in [
            "user_pass",
            "pass",
            "pw",
            "user_pw",
            "pword",
            "user_activation_key",
            "activationKey",
            "reset_key",
            "invite_code",
            "confirmation_code",
            "passwort",
            "Kennwort",
            "contraseña",
            "contrasena",
            "mot_de_passe",
            "motDePasse",
            "senha",
            "parola",
            "wachtwoord",
            "mat_khau",
            "matkhau",
            "MatKhau",
            "пароль",
            "Пароль",
        ] {
            assert!(is_sensitive_name(name), "{name}");
        }
        for name in [
            "passed",
            "bypass",
            "compass",
            "passenger",
            "pwm_duty",
            "reset_at",
            "invite_count",
            "parole_date",
        ] {
            assert!(!is_sensitive_name(name), "{name}");
        }
        for hash in [
            // WordPress before and after 6.8, Drupal 7, phpBB, Apache, Sun MD5, NetBSD SHA-1, Spring, LDAP.
            "$P$BxvVKjpM3qHo1rBv4bAEHdN8E1GCNb.",
            "$wp$2y$10$DXbXQ3ZbXJhsW0MGsJ8u5.PVu5H3WkNnqvU/9.L.hJ1i8xGkN4vOe",
            "$S$DTvYhlR0Abcm9uEX1Q2W3E4R5T6Y7U8I9O0P1A2S3D4F5G6H7J8K9L0",
            "$H$9IQRaTwmfeRo7ud9Fh4E2PdI0S3r.L0",
            "$apr1$r31.....$HqJZimcKQFAMYayBlzkrA/",
            "$md5,rounds=5000$GUBv0xjJ$$mSwgIswdjlTY0YxV7HBVm0",
            "$sha1$40000$jtNX3nZ2$hBNaIXkt4wBI2o5rsi8KejSjNqIq",
            "{bcrypt}$2a$10$dXJ3SW6G7P50lGmMkkmwe.20cQQubK3.HZWzG3YB1tlRy.fqvM/BG",
            "{noop}password",
            "{pbkdf2@SpringSecurity_v5_8}5d923b44a6d129f3ddf3e3c8d29412723dcbde72445e8ef6bf3b508fbf17fa4ed4d6b99ca763d8dc",
            "{scrypt}$e0801$8bWJaSu2IKSn9Z9kM+TPXfOc/9bdYSrN1oD9qfVThWEwdRTnO7re7Ei+fUZRJ68k9lTyuTeUp4of4g24hHnazw==$OAOec05+bXxvuu/1qZ6NUR+xQYvYv7BeL1QxwRpY5Pc=",
            "{MD5}X03MO1qnZdYdgyfeuILPmQ==",
            "$6$rounds=5000$saltsalt$hashhashhashhashhash",
        ] {
            assert_eq!(scrub(hash), REDACTED, "{hash}");
        }
        assert_eq!(
            scrub("1700000000:$P$BxvVKjpM3qHo1rBv4bAEHdN8E1GCNb."),
            format!("1700000000:{REDACTED}")
        );
        // A `$` around a token does not hide it.
        assert_eq!(scrub("$sk-abcdefghijklmnopqrstuvwx1234$"), REDACTED);
        for text in ["costs $25 today", "$1,000 or $2", "SELECT $1, $2"] {
            assert_eq!(scrub(text), text);
        }
        // A WordPress row read by `SELECT * FROM wp_users`.
        assert_eq!(
            redact_cell("user_pass", &Value::Text("$P$Bxv".into())),
            Value::Text(REDACTED.into())
        );
        assert_eq!(
            redact_cell("user_activation_key", &Value::Text("1700000000:$P$B".into())),
            Value::Text(REDACTED.into())
        );
    }

    #[test]
    fn json_kept_as_a_string_is_redacted_inside() {
        let document = serde_json::json!({
            "path": "/login",
            "body": "{\"email\":\"a@b.c\",\"password\":\"hunter2\"}",
            "deeper": "{\"inner\":\"{\\\"token\\\":\\\"abc\\\"}\"}",
            "plain": "{not json}",
        });
        let redacted = redact_json(&document);
        let text = redacted.to_string();
        assert!(!text.contains("hunter2") && !text.contains("abc"), "{text}");
        let body: Json = serde_json::from_str(redacted["body"].as_str().unwrap()).unwrap();
        assert_eq!(body, serde_json::json!({ "email": "a@b.c", "password": REDACTED }));
        assert_eq!(redacted["plain"], "{not json}");
        // A JSON column holding JSON text in a field, and a cut body that no longer parses.
        let Value::Json(cell) = redact_cell("payload", &Value::Json(r#"{"raw": "{\"pw\": \"x1\"}"}"#.into())) else {
            panic!("JSON stays JSON");
        };
        assert!(!cell.contains("x1"), "{cell}");
        assert!(!redact_documents_text(r#"{"body": "{\"password\":\"hunter2\",\"a\":\"…"}"#).contains("hunter2"));
    }

    #[test]
    fn values_of_sensitive_keys_in_text_are_redacted() {
        assert_eq!(
            scrub("Server=db;User Id=app;Password=Hunter2;"),
            format!("Server=db;User Id=app;Password={REDACTED};")
        );
        assert_eq!(
            scrub("https://api.example.com/hook?password=Hunter2&user=x"),
            format!("https://api.example.com/hook?password={REDACTED}&user=x")
        );
        assert_eq!(scrub("DB_PASSWORD: Hunter2"), format!("DB_PASSWORD: {REDACTED}"));
        assert_eq!(scrub("DB_PASSWORD = Hunter2 next"), format!("DB_PASSWORD = {REDACTED} next"));
        assert_eq!(scrub("psql --password=Hunter2 -h db"), format!("psql --password={REDACTED} -h db"));
        assert_eq!(
            scrub("docker run -e POSTGRES_PASSWORD=Hunter2 postgres"),
            format!("docker run -e POSTGRES_PASSWORD={REDACTED} postgres")
        );
        assert_eq!(
            scrub(r#"{"password":"hunter2","a":1}"#),
            format!(r#"{{"password":"{REDACTED}","a":1}}"#)
        );
        assert_eq!(
            scrub("api_key=ab:cd@ef;x=1"),
            format!("api_key={REDACTED}:{REDACTED}@{REDACTED};x=1")
        );
        for text in [
            "status:paid,new",
            "a=b&c=d",
            "ratio 3:2",
            "http://localhost:8080/path",
            "sort=asc&page=2",
            "session:{id}",
            "token:{id}:meta",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn cut_secrets_are_still_recognised() {
        let jwt = format!("eyJ{}…", "a1B2".repeat(50));
        assert_eq!(scrub(&jwt), REDACTED);
        assert_eq!(scrub(&format!("Bearer {}…", "a1B2".repeat(50))), format!("Bearer {REDACTED}"));
        assert_eq!(scrub(&format!("{}…", "f0".repeat(30))), REDACTED);
        let sample = format!(r#"{{"headers": {{"authorization": "Bearer {}…"}}}}"#, "a1B2".repeat(50));
        assert!(!redact_documents_text(&sample).contains("a1B2"), "{sample}");
    }

    #[test]
    fn people_and_identity_numbers_are_personal() {
        for name in [
            "login",
            "user",
            "author",
            "created_by",
            "updatedBy",
            "owner",
            "contact",
            "emergency_contact",
            "mobile",
            "tel",
            "mail",
            "tax_id",
            "national_id",
            "account_number",
            "acct_no",
            "assignee",
            "postcode",
        ] {
            assert!(is_personal_name(name), "{name}");
        }
        for name in [
            "user_type",
            "owner_kind",
            "login_count",
            "mobile_app_version",
            "account_type",
            "created_at",
            "status",
        ] {
            assert!(!is_personal_name(name), "{name}");
        }
    }

    #[test]
    fn key_patterns_are_checked_again() {
        assert_eq!(key_pattern("user:{id}:profile"), "user:{id}:profile");
        assert_eq!(key_pattern("otp:+15551234567"), "otp:{phone}");
        assert_eq!(key_pattern("rate:203.0.113.5:hits"), "rate:{ip}:hits");
        assert_eq!(key_pattern("mail/ann@example.com"), "mail/{email}");
        assert_eq!(key_pattern("k:sk-abcdefghijklmnopqrstuvwx1234"), "k:{id}");
        assert_eq!(key_pattern("s:Zm9vYmFy+cXV4=="), "s:{token}");
        assert_eq!(key_pattern("…"), "…");
        assert_eq!(key_pattern("{binary}"), "{binary}");
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
