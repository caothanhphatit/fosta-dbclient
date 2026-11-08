//! The Redis engine port (planned): descriptor, classification and connection URLs are wired; `connect` returns
//! `ENGINE_NOT_WIRED` until a `Connection` over [`super::connection::RedisEngine`] (the core) is implemented here.
//!
//! Engine views call these `invoke` names: `redis.info`, `redis.slowlog`, `redis.scan`, `redis.keyInfo`,
//! `redis.readValue` (reads) and `redis.edit` (a [`RedisEdit`], classified by [`RedisEdit::class`]).

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{
    classify::{self, CommandClass},
    connection::{RedisConnectOptions, DEFAULT_PORT},
    console::{split_lines, tokenize},
    descriptor,
    edit::RedisEdit,
};
use crate::{
    contract::{
        classify_common, not_wired, AuthMethodId, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec,
        Engine, EngineDescriptor, ParsedConnection, StatementInfo, TlsConfig, TlsMode,
    },
    error::AppResult,
};

pub struct RedisAdapter;

pub static REDIS: RedisAdapter = RedisAdapter;

/// A core command class as a gateway category; `None` for commands the console cannot run (Pub/Sub, MONITOR).
/// Session commands (SELECT, MULTI/EXEC, CLIENT SETNAME) only change the console's connection, so they count as reads.
fn category(class: CommandClass) -> Option<Category> {
    match class {
        CommandClass::Session | CommandClass::Read => Some(Category::Read),
        CommandClass::Write => Some(Category::Write),
        CommandClass::Unknown => Some(Category::Unknown),
        CommandClass::Admin => Some(Category::Admin),
        CommandClass::Destructive => Some(Category::Destructive),
        CommandClass::Unsupported => None,
    }
}

/// Console text: one command per line (comments and blank lines skipped), each tokenized like redis-cli.
fn classify_console(text: &str) -> Classification {
    let statements = split_lines(text)
        .into_iter()
        .map(|(line_number, line)| match tokenize(line) {
            Ok(args) => {
                let command = classify::classify(&args);
                match category(command.class) {
                    Some(category) => {
                        let mut info = StatementInfo::new(category, &command.command, line);
                        info.reason = command.reason;
                        info
                    }
                    None => StatementInfo::refused(
                        &command.command,
                        line,
                        command.reason.as_deref().unwrap_or("The console cannot run this command."),
                    ),
                }
            }
            Err(error) => StatementInfo::new(Category::Unknown, "command", line).with_reason(&format!("Line {line_number}: {error}")),
        })
        .collect();
    Classification { statements }
}

/// A `redis.edit` payload, classified by the edit itself; malformed payloads are refused.
fn classify_edit(name: &str, args: &serde_json::Value) -> StatementInfo {
    let edit: RedisEdit = match serde_json::from_value(args.clone()) {
        Ok(edit) => edit,
        Err(error) => return StatementInfo::refused(name, name, &format!("invalid arguments: {error}")),
    };
    let op = args.get("op").and_then(|op| op.as_str()).unwrap_or_default();
    let keys: Vec<String> = match &edit {
        RedisEdit::Delete { keys } => keys.iter().map(|key| key.display()).collect(),
        other => other.key().map(|key| key.display()).into_iter().collect(),
    };
    let reason = match &edit {
        RedisEdit::Delete { .. } => Some("Deletes whole keys."),
        RedisEdit::Rename { overwrite: true, .. } => Some("Replaces the target key if it exists."),
        _ => None,
    };
    let category = category(edit.class()).unwrap_or(Category::Unknown);
    let mut info = StatementInfo::new(category, &format!("{name} {op}"), &format!("{name} {op} {}", keys.join(" "))).with_tables(keys);
    info.reason = reason.map(str::to_owned);
    info
}

fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    match name {
        "redis.info" | "redis.slowlog" | "redis.scan" | "redis.keyInfo" | "redis.readValue" => {
            StatementInfo::new(Category::Read, name, &format!("{name} {args}"))
        }
        "redis.edit" => classify_edit(name, args),
        _ => StatementInfo::refused(name, name, "unknown operation"),
    }
}

#[async_trait]
impl Engine for RedisAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "redis", text } => classify_console(text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("Redis consoles do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, args } => Classification::single(classify_capability(name, args)),
            _ => Classification::default(),
        }
    }

    /// `redis://[user[:password]@]host[:port][/db]` or `rediss://…`, through [`RedisConnectOptions::from_url`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = RedisConnectOptions::from_url(text)?;
        let mut options = HashMap::new();
        if parsed.db != 0 {
            options.insert("db".to_owned(), parsed.db.to_string());
        }
        let auth = match (&parsed.username, &parsed.password) {
            (Some(_), _) => Some(AuthMethodId::AclUser),
            (None, Some(_)) => Some(AuthMethodId::Password),
            (None, None) => None,
        };
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port.unwrap_or(DEFAULT_PORT),
                user: parsed.username.unwrap_or_default(),
                options,
                tls: parsed.tls.map(|_| TlsConfig {
                    mode: TlsMode::VerifyFull,
                    ..Default::default()
                }),
                auth,
                ..Default::default()
            },
            password: parsed.password,
        })
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        Err(not_wired(descriptor::ID, descriptor::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::contract::{errors::ENGINE_NOT_WIRED, ConnectSecrets, Endpoint, EngineStatus};

    fn console(text: &str) -> Classification {
        REDIS.classify(&ClassifyInput::Statement { language: "redis", text })
    }

    fn capability(name: &str, args: serde_json::Value) -> Classification {
        REDIS.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptor_is_planned_and_consistent() {
        let descriptor = REDIS.descriptor();
        assert_eq!(REDIS.id(), "redis");
        assert_eq!(descriptor.status, EngineStatus::Planned);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("redis"));
        assert!(descriptor.filter.raw.is_none());
    }

    #[test]
    fn console_lines_are_classified_one_by_one() {
        assert_eq!(console("GET user:1").worst(), Some(Category::Read));
        assert_eq!(console("SELECT 2").worst(), Some(Category::Read));
        assert_eq!(console("HSET user:1 name \"Ada Lovelace\"").worst(), Some(Category::Write));
        assert_eq!(console("FLUSHALL").worst(), Some(Category::Destructive));
        assert_eq!(console("CONFIG SET maxmemory 1gb").worst(), Some(Category::Admin));
        let script = console("# warm up\nGET a\n\nDEL a b\n");
        assert_eq!(script.statements.len(), 2);
        assert_eq!(script.statements[1].kind, "DEL");
        assert_eq!(console("SET k \"unclosed").worst(), Some(Category::Unknown));
    }

    #[test]
    fn streaming_commands_are_refused() {
        let classification = console("GET a\nSUBSCRIBE news");
        assert!(classification.refusal().is_some());
        assert_eq!(classification.statements[1].kind, "SUBSCRIBE");
        assert!(console("MONITOR").refusal().is_some());
    }

    #[test]
    fn engine_view_operations_are_classified_and_unknown_ones_refused() {
        assert_eq!(
            capability("redis.scan", json!({ "cursor": "0", "pattern": "user:*" })).worst(),
            Some(Category::Read)
        );
        let set = capability(
            "redis.edit",
            json!({ "op": "setString", "key": { "text": "k" }, "value": { "text": "v" }, "ttlMs": null, "keepTtl": false, "condition": "always" }),
        );
        assert_eq!(set.worst(), Some(Category::Write));
        assert_eq!(set.tables(), vec!["k".to_owned()]);
        let delete = capability("redis.edit", json!({ "op": "delete", "keys": [{ "text": "a" }, { "text": "b" }] }));
        assert_eq!(delete.worst(), Some(Category::Destructive));
        assert_eq!(delete.tables().len(), 2);
        assert!(capability("redis.edit", json!({ "op": "teleport" })).refusal().is_some());
        assert_eq!(capability("redis.flush", json!({})).refusal(), Some("unknown operation"));
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = REDIS.parse_connection_string("rediss://app:s3cret@cache.local:6380/3").unwrap();
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.spec.engine, "redis");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("cache.local", 6380));
        assert_eq!(parsed.spec.user, "app");
        assert_eq!(parsed.spec.option("db"), Some("3"));
        assert_eq!(parsed.spec.auth, Some(AuthMethodId::AclUser));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("s3cret")));
        assert!(parsed.spec.url.is_none());
        let plain = REDIS.parse_connection_string("redis://localhost").unwrap();
        assert_eq!(plain.spec.port, 6379);
        assert!(plain.spec.options.is_empty() && plain.spec.tls.is_none() && plain.password.is_none());
        assert!(REDIS.parse_connection_string("http://localhost").is_err());
    }

    #[tokio::test]
    async fn connect_is_not_wired_yet() {
        let request = ConnectRequest {
            spec: ConnectionSpec::default(),
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 1,
            },
            connect_timeout: Duration::from_secs(1),
        };
        let Err(error) = REDIS.connect(request).await else {
            panic!("a planned engine must not connect");
        };
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
    }
}
