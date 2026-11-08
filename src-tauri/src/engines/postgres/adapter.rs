//! The PostgreSQL engine port (planned): descriptor, classification and connection strings are wired; `connect` returns
//! `ENGINE_NOT_WIRED` until a `Connection` over [`super::PgEngine`] is implemented here.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{descriptor, PgOptions};
use crate::{
    contract::{
        classify_common, not_wired, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine,
        EngineDescriptor, ParsedConnection, StatementInfo,
    },
    engines::common::{adapt::tls_to_contract, classify, split::Dialect},
    error::AppResult,
};

pub struct PostgresAdapter;

pub static POSTGRES: PostgresAdapter = PostgresAdapter;

#[async_trait]
impl Engine for PostgresAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::Postgres, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("PostgreSQL sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    /// `postgres://user:pass@host:port/db?sslmode=…` or a libpq key/value string, through [`PgOptions::parse`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = PgOptions::parse(text)?;
        let defaults = PgOptions::default();
        let mut options = HashMap::new();
        if parsed.application_name != defaults.application_name {
            options.insert("applicationName".to_owned(), parsed.application_name.clone());
        }
        if parsed.connect_timeout != defaults.connect_timeout {
            options.insert("connectTimeout".to_owned(), parsed.connect_timeout.as_secs().to_string());
        }
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port,
                user: parsed.user,
                database: Some(parsed.database),
                tls: tls_to_contract(&parsed.tls),
                options,
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

    use super::*;
    use crate::contract::{errors::ENGINE_NOT_WIRED, ConnectSecrets, Endpoint, EngineStatus, TlsMode};

    fn statement(text: &str) -> Classification {
        POSTGRES.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    #[test]
    fn descriptor_is_planned_and_consistent() {
        let descriptor = POSTGRES.descriptor();
        assert_eq!(POSTGRES.id(), "postgres");
        assert_eq!(descriptor.status, EngineStatus::Planned);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
    }

    #[test]
    fn statements_are_classified_with_the_postgres_dialect() {
        assert_eq!(statement("SELECT * FROM orders").worst(), Some(Category::Read));
        assert_eq!(
            statement("UPDATE orders SET paid = true WHERE id = 1").worst(),
            Some(Category::Write)
        );
        assert_eq!(statement("CREATE TABLE t (id int)").worst(), Some(Category::Ddl));
        assert_eq!(statement("DELETE FROM orders").worst(), Some(Category::Destructive));
        assert_eq!(statement("SELECT 1; DROP TABLE t").statements.len(), 2);
        let other = POSTGRES.classify(&ClassifyInput::Statement {
            language: "redis",
            text: "GET k",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
    }

    #[test]
    fn every_capability_name_is_refused() {
        let args = serde_json::Value::Null;
        let classification = POSTGRES.classify(&ClassifyInput::Capability {
            name: "pg.vacuum",
            args: &args,
        });
        assert_eq!(classification.refusal(), Some("unknown operation"));
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = POSTGRES
            .parse_connection_string("postgres://app:s3cret@db.local:5433/shop?sslmode=require&application_name=etl")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.spec.engine, "postgres");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 5433));
        assert_eq!(parsed.spec.user, "app");
        assert_eq!(parsed.spec.database.as_deref(), Some("shop"));
        assert_eq!(parsed.spec.option("applicationName"), Some("etl"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::Require));
        assert!(parsed.spec.options.values().all(|value| !value.contains("s3cret")));
        assert!(parsed.spec.url.is_none());
        assert!(POSTGRES.parse_connection_string("postgres://h/db?bogus=1").is_err());
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
        let Err(error) = POSTGRES.connect(request).await else {
            panic!("a planned engine must not connect");
        };
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
    }
}
