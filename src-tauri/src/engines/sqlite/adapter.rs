//! The SQLite engine port (planned): descriptor, classification and connection strings are wired; `connect` returns
//! `ENGINE_NOT_WIRED` until a `Connection` over [`super::SqliteEngine`] (the core) is implemented here.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{descriptor, SqliteOptions};
use crate::{
    contract::{
        classify_common, not_wired, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine,
        EngineDescriptor, ParsedConnection, StatementInfo,
    },
    engines::common::{classify, split::Dialect},
    error::AppResult,
};

pub struct SqliteAdapter;

pub static SQLITE: SqliteAdapter = SqliteAdapter;

#[async_trait]
impl Engine for SqliteAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::Sqlite, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("SQLite sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    /// `sqlite:///path.db`, `file:path.db?mode=ro` or a plain path, through [`SqliteOptions::from_uri`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = SqliteOptions::from_uri(text)?;
        let defaults = SqliteOptions::file(&parsed.path);
        let mut options = HashMap::new();
        if parsed.read_only {
            options.insert("readOnly".to_owned(), "true".to_owned());
        }
        if parsed.create {
            options.insert("create".to_owned(), "true".to_owned());
        }
        if parsed.busy_timeout != defaults.busy_timeout {
            options.insert("busyTimeout".to_owned(), parsed.busy_timeout.as_millis().to_string());
        }
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                file: Some(parsed.path),
                options,
                ..Default::default()
            },
            password: None,
        })
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        Err(not_wired(descriptor::ID, descriptor::LABEL))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{errors::ENGINE_NOT_WIRED, ConnectSecrets, Endpoint, EngineStatus};

    fn statement(text: &str) -> Classification {
        SQLITE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    #[test]
    fn descriptor_is_planned_and_consistent() {
        let descriptor = SQLITE.descriptor();
        assert_eq!(SQLITE.id(), "sqlite");
        assert_eq!(descriptor.status, EngineStatus::Planned);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        assert!(descriptor.connection.uses_file && !descriptor.connection.uses_host);
        assert!(descriptor
            .connection
            .fields
            .iter()
            .any(|field| field.key == "file" && field.required));
    }

    #[test]
    fn statements_are_classified_with_the_sqlite_dialect() {
        assert_eq!(statement("SELECT * FROM orders").worst(), Some(Category::Read));
        assert_eq!(statement("INSERT INTO orders (id) VALUES (1)").worst(), Some(Category::Write));
        assert_eq!(statement("CREATE INDEX i ON orders (id)").worst(), Some(Category::Ddl));
        assert_eq!(statement("DROP TABLE orders").worst(), Some(Category::Destructive));
        let other = SQLITE.classify(&ClassifyInput::Statement {
            language: "es-console",
            text: "GET /",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
    }

    #[test]
    fn every_capability_name_is_refused() {
        let args = serde_json::Value::Null;
        let classification = SQLITE.classify(&ClassifyInput::Capability {
            name: "sqlite.vacuum",
            args: &args,
        });
        assert_eq!(classification.refusal(), Some("unknown operation"));
    }

    #[test]
    fn uris_become_a_file_and_options() {
        let parsed = SQLITE
            .parse_connection_string("file:///C:/data/shop.db?mode=ro&busy_timeout=250")
            .unwrap();
        assert_eq!(parsed.spec.engine, "sqlite");
        assert_eq!(parsed.spec.file.as_deref(), Some("C:/data/shop.db"));
        assert_eq!(parsed.spec.option("readOnly"), Some("true"));
        assert_eq!(parsed.spec.option("busyTimeout"), Some("250"));
        assert!(parsed.password.is_none() && parsed.spec.url.is_none());
        assert_eq!(SQLITE.parse_connection_string("shop.db").unwrap().spec.options.len(), 0);
        assert!(SQLITE.parse_connection_string("   ").is_err());
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
        let Err(error) = SQLITE.connect(request).await else {
            panic!("a planned engine must not connect");
        };
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
    }
}
