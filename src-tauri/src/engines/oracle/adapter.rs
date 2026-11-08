//! The Oracle engine port (planned): descriptor, classification and connection strings are wired; `connect` returns
//! `ENGINE_NOT_WIRED` until a `Connection` over [`super::OraEngine`] is implemented here.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{descriptor, OraOptions};
use crate::{
    contract::{
        classify_common, not_wired, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine,
        EngineDescriptor, ParsedConnection, StatementInfo,
    },
    engines::common::{adapt::tls_to_contract, classify, split::Dialect},
    error::AppResult,
};

pub struct OracleAdapter;

pub static ORACLE: OracleAdapter = OracleAdapter;

#[async_trait]
impl Engine for OracleAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::Oracle, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("Oracle sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    /// `oracle://user:pass@host:1521/SERVICE`, `user/pass@//host:1521/SERVICE`, `user/pass@alias` or EZConnect, through
    /// [`OraOptions::parse`]. A descriptor or alias lands in `options.connectString`.
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = OraOptions::parse(text)?;
        let defaults = OraOptions::default();
        let mut options = HashMap::new();
        if parsed.connect_timeout != defaults.connect_timeout {
            options.insert("connectTimeout".to_owned(), parsed.connect_timeout.as_secs().to_string());
        }
        let database = match parsed.connect_string {
            Some(connect_string) => {
                options.insert("connectString".to_owned(), connect_string);
                None
            }
            None => Some(parsed.service),
        };
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port,
                user: parsed.user,
                database,
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
        ORACLE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    #[test]
    fn descriptor_is_planned_and_consistent() {
        let descriptor = ORACLE.descriptor();
        assert_eq!(ORACLE.id(), "oracle");
        assert_eq!(descriptor.status, EngineStatus::Planned);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
    }

    #[test]
    fn statements_are_classified_with_the_oracle_rules() {
        assert_eq!(statement("SELECT * FROM dual").worst(), Some(Category::Read));
        assert_eq!(
            statement("UPDATE orders SET status = 'PAID' WHERE id = 1").worst(),
            Some(Category::Write)
        );
        assert_eq!(statement("CREATE TABLE t (id NUMBER)").worst(), Some(Category::Ddl));
        assert_eq!(statement("TRUNCATE TABLE orders").worst(), Some(Category::Destructive));
    }

    #[test]
    fn every_capability_name_is_refused() {
        let args = serde_json::Value::Null;
        let classification = ORACLE.classify(&ClassifyInput::Capability {
            name: "oracle.awr",
            args: &args,
        });
        assert_eq!(classification.refusal(), Some("unknown operation"));
    }

    #[test]
    fn connect_strings_split_into_a_spec_and_a_password() {
        let parsed = ORACLE
            .parse_connection_string("oracle://scott:tiger@db.local:1522/ORCLPDB1?ssl=true")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("tiger"));
        assert_eq!(parsed.spec.engine, "oracle");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 1522));
        assert_eq!(parsed.spec.user, "scott");
        assert_eq!(parsed.spec.database.as_deref(), Some("ORCLPDB1"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("tiger")));
        assert!(parsed.spec.url.is_none());

        let alias = ORACLE.parse_connection_string("scott/tiger@PRODDB").unwrap();
        assert_eq!(alias.spec.option("connectString"), Some("PRODDB"));
        assert!(alias.spec.database.is_none());
        assert_eq!(alias.password.as_deref(), Some("tiger"));
        assert!(ORACLE.parse_connection_string("db.local:1521/FREEPDB1").is_err());
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
        let Err(error) = ORACLE.connect(request).await else {
            panic!("a planned engine must not connect");
        };
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
    }
}
