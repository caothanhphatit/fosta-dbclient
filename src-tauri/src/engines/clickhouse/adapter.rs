//! The ClickHouse engine port (planned): descriptor, classification and connection strings are wired; `connect` returns
//! `ENGINE_NOT_WIRED` until a `Connection` over [`super::ChEngine`] is implemented here.
//!
//! Engine views call these `invoke` names: `clickhouse.parts`, `clickhouse.mutations`, `clickhouse.queryLog` (reads) and
//! `clickhouse.kill` (admin).

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{descriptor, ChOptions};
use crate::{
    contract::{
        classify_common, not_wired, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine,
        EngineDescriptor, ParsedConnection, StatementInfo,
    },
    engines::common::{adapt::tls_to_contract, classify, split::Dialect},
    error::AppResult,
};

pub struct ClickHouseAdapter;

pub static CLICKHOUSE: ClickHouseAdapter = ClickHouseAdapter;

/// The classification of an engine-view operation; unknown names are refused.
fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    let excerpt = format!("{name} {args}");
    match name {
        "clickhouse.parts" | "clickhouse.mutations" | "clickhouse.queryLog" => StatementInfo::new(Category::Read, name, &excerpt),
        "clickhouse.kill" => StatementInfo::new(Category::Admin, name, &excerpt).with_reason("Stops a running query or mutation."),
        _ => StatementInfo::refused(name, name, "unknown operation"),
    }
}

#[async_trait]
impl Engine for ClickHouseAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::ClickHouse, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("ClickHouse sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, args } => Classification::single(classify_capability(name, args)),
            _ => Classification::default(),
        }
    }

    /// `clickhouse://user:pass@host:port/db?secure=true` or `http(s)://host:port/?user=…`, through [`ChOptions::parse`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = ChOptions::parse(text)?;
        let mut options = HashMap::new();
        if parsed.connect_timeout != ChOptions::default().connect_timeout {
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

    use serde_json::json;

    use super::*;
    use crate::contract::{errors::ENGINE_NOT_WIRED, ConnectSecrets, Endpoint, EngineStatus, TlsMode};

    fn statement(text: &str) -> Classification {
        CLICKHOUSE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    fn capability(name: &str) -> Classification {
        let args = json!({ "queryId": "q1" });
        CLICKHOUSE.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptor_is_planned_and_consistent() {
        let descriptor = CLICKHOUSE.descriptor();
        assert_eq!(CLICKHOUSE.id(), "clickhouse");
        assert_eq!(descriptor.status, EngineStatus::Planned);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        assert!(!descriptor.capabilities.contains(crate::contract::Capability::Transactions));
    }

    #[test]
    fn statements_are_classified_with_the_clickhouse_dialect() {
        assert_eq!(statement("SELECT count() FROM events").worst(), Some(Category::Read));
        assert_eq!(statement("INSERT INTO events VALUES (1)").worst(), Some(Category::Write));
        assert_eq!(
            statement("CREATE TABLE t (id UInt64) ENGINE = MergeTree ORDER BY id").worst(),
            Some(Category::Ddl)
        );
        assert_eq!(statement("TRUNCATE TABLE events").worst(), Some(Category::Destructive));
        assert_eq!(statement("ALTER TABLE events DELETE WHERE id = 1").worst(), Some(Category::Write));
    }

    #[test]
    fn engine_view_operations_are_classified_and_unknown_ones_refused() {
        assert_eq!(capability("clickhouse.parts").worst(), Some(Category::Read));
        assert_eq!(capability("clickhouse.queryLog").refusal(), None);
        assert_eq!(capability("clickhouse.kill").worst(), Some(Category::Admin));
        assert_eq!(capability("clickhouse.dropEverything").refusal(), Some("unknown operation"));
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = CLICKHOUSE
            .parse_connection_string("clickhouse://analyst:p%40ss@ch.local/metrics?secure=true&connect_timeout=3")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("p@ss"));
        assert_eq!(parsed.spec.engine, "clickhouse");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("ch.local", 8443));
        assert_eq!(parsed.spec.user, "analyst");
        assert_eq!(parsed.spec.database.as_deref(), Some("metrics"));
        assert_eq!(parsed.spec.option("connectTimeout"), Some("3"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("p@ss")));
        assert!(parsed.spec.url.is_none());
        let http = CLICKHOUSE.parse_connection_string("http://localhost:8123/?password=x").unwrap();
        assert_eq!(http.password.as_deref(), Some("x"));
        assert!(http.spec.options.is_empty() && http.spec.tls.is_none());
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
        let Err(error) = CLICKHOUSE.connect(request).await else {
            panic!("a planned engine must not connect");
        };
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
    }
}
