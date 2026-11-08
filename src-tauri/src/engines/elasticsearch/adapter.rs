//! The Elasticsearch and OpenSearch engine ports (planned): one adapter type, two statics. Descriptor, classification and
//! connection strings are wired; `connect` returns `ENGINE_NOT_WIRED` until a `Connection` over
//! [`super::connection::EsEngine`] is implemented here.
//!
//! Engine views call these `invoke` names: `es.indexDetails`, `es.clusterHealth`, `es.cat`, `es.document` (reads),
//! `es.documentOp` (a [`DocumentOp`]) and `es.byQuery` (a [`ByQueryOp`]), both classified by their own `class()`.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use reqwest::{Method, Url};
use serde_json::json;

use super::{
    classify::{classify, RequestClass, RequestClassification},
    client::EsBody,
    config::{parse_cloud_id, EsConnectOptions},
    console,
    descriptor::{self, Flavor, DEFAULT_PORT},
    docs::{ByQueryKind, ByQueryOp, DocumentOp},
};
use crate::{
    contract::{
        classify_common, not_wired, value::percent_decode, AuthMethodId, Category, Classification, ClassifyInput, ConnectRequest,
        Connection, ConnectionSpec, Engine, EngineDescriptor, ParsedConnection, StatementInfo, TlsConfig, TlsMode,
    },
    error::{AppError, AppResult},
};

pub struct ElasticAdapter {
    flavor: Flavor,
}

pub static ELASTICSEARCH: ElasticAdapter = ElasticAdapter {
    flavor: Flavor::Elasticsearch,
};

pub static OPENSEARCH: ElasticAdapter = ElasticAdapter {
    flavor: Flavor::OpenSearch,
};

impl ElasticAdapter {
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    fn classify_statement(&self, language: &str, text: &str) -> Classification {
        match language {
            "es-console" => classify_console(text),
            "esql" if self.flavor == Flavor::Elasticsearch => Classification::single(request_info(
                classify(&Method::POST, "/_query", Some(&json!({ "query": text }))),
                text,
            )),
            "es-sql" => Classification::single(request_info(
                classify(&Method::POST, self.flavor.sql_path(), Some(&json!({ "query": text }))),
                text,
            )),
            _ => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("{} sessions do not run the '{language}' language.", self.flavor.label())),
            ),
        }
    }

    /// A comma-separated node list (`https://user:pass@es1:9200, es2:9200`) or, for Elasticsearch, a Cloud ID.
    fn parse(&self, text: &str) -> AppResult<ParsedConnection> {
        let text = text.trim();
        if self.flavor == Flavor::Elasticsearch && !text.contains("://") {
            if let Ok(endpoint) = parse_cloud_id(text) {
                return Ok(self.cloud_spec(text, &endpoint));
            }
        }
        let mut urls = Vec::new();
        let (mut user, mut password) = (None, None);
        for node in text.split(',').map(str::trim).filter(|node| !node.is_empty()) {
            let full = if node.contains("://") {
                node.to_owned()
            } else {
                format!("http://{node}")
            };
            let mut url = Url::parse(&full).map_err(|error| AppError::new(format!("`{node}` is not a valid URL: {error}")))?;
            if !url.username().is_empty() {
                user.get_or_insert_with(|| percent_decode(url.username()));
            }
            if let Some(secret) = url.password() {
                password.get_or_insert_with(|| percent_decode(secret));
            }
            // Credentials go to the spec's user and the secret store, never into the stored URL.
            let _ = url.set_username("");
            let _ = url.set_password(None);
            urls.push(url.as_str().trim_end_matches('/').to_owned());
        }
        let urls = EsConnectOptions {
            urls,
            ..Default::default()
        }
        .base_urls()?;
        let first = Url::parse(&urls[0]).map_err(|error| AppError::new(error.to_string()))?;
        // A node list (or a proxy path prefix) only fits in `url`; a single plain node is host and port.
        let url = (urls.len() > 1 || first.path() != "/").then(|| urls.join(","));
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: self.flavor.id().into(),
                host: first.host_str().unwrap_or_default().trim_matches(['[', ']']).to_owned(),
                port: first.port_or_known_default().unwrap_or(DEFAULT_PORT),
                auth: user.is_some().then_some(AuthMethodId::Password),
                user: user.unwrap_or_default(),
                url,
                tls: (first.scheme() == "https").then(verified_tls),
                ..Default::default()
            },
            password,
        })
    }

    fn cloud_spec(&self, cloud_id: &str, endpoint: &str) -> ParsedConnection {
        let parsed = Url::parse(endpoint).ok();
        let mut options = HashMap::new();
        options.insert("cloudId".to_owned(), cloud_id.to_owned());
        ParsedConnection {
            spec: ConnectionSpec {
                engine: self.flavor.id().into(),
                host: parsed
                    .as_ref()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_default(),
                port: parsed.as_ref().and_then(Url::port_or_known_default).unwrap_or(443),
                options,
                tls: Some(verified_tls()),
                ..Default::default()
            },
            password: None,
        }
    }
}

fn verified_tls() -> TlsConfig {
    TlsConfig {
        mode: TlsMode::VerifyFull,
        ..Default::default()
    }
}

fn category(class: RequestClass) -> Category {
    match class {
        RequestClass::Read => Category::Read,
        RequestClass::Write => Category::Write,
        RequestClass::Ddl => Category::Ddl,
        RequestClass::Admin => Category::Admin,
        RequestClass::Destructive => Category::Destructive,
    }
}

fn request_info(classification: RequestClassification, excerpt: &str) -> StatementInfo {
    StatementInfo::new(category(classification.class), &classification.api, excerpt)
        .with_tables(classification.target.into_iter().collect())
}

/// Console text: each request is classified from its method, path and body.
fn classify_console(text: &str) -> Classification {
    match console::parse(text) {
        Ok(requests) => Classification {
            statements: requests
                .into_iter()
                .map(|request| {
                    let body = match &request.body {
                        Some(EsBody::Json(value)) => value.to_string(),
                        Some(EsBody::NdJson(values)) => values.iter().map(|value| value.to_string()).collect::<Vec<_>>().join(" "),
                        Some(EsBody::Text(text)) => text.clone(),
                        None => String::new(),
                    };
                    request_info(request.classification, &format!("{} {} {body}", request.method, request.path))
                })
                .collect(),
        },
        Err(error) => Classification::single(StatementInfo::new(Category::Unknown, "request", text).with_reason(&error.to_string())),
    }
}

fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    let excerpt = format!("{name} {args}");
    match name {
        "es.indexDetails" | "es.clusterHealth" | "es.cat" | "es.document" => {
            let index = args.get("index").and_then(|index| index.as_str()).map(str::to_owned);
            StatementInfo::new(Category::Read, name, &excerpt).with_tables(index.into_iter().collect())
        }
        "es.documentOp" => match serde_json::from_value::<DocumentOp>(args.clone()) {
            Ok(op) => {
                let index = match &op {
                    DocumentOp::Index { index, .. }
                    | DocumentOp::Update { index, .. }
                    | DocumentOp::UpdateScript { index, .. }
                    | DocumentOp::Delete { index, .. } => index.clone(),
                };
                let kind = format!("{name} {}", args.get("op").and_then(|op| op.as_str()).unwrap_or_default());
                StatementInfo::new(category(op.class()), &kind, &excerpt).with_tables(vec![index])
            }
            Err(error) => StatementInfo::refused(name, name, &format!("invalid arguments: {error}")),
        },
        "es.byQuery" => match serde_json::from_value::<ByQueryOp>(args.clone()) {
            Ok(op) => {
                let (kind, reason) = match op.kind {
                    ByQueryKind::Update => ("update by query", None),
                    ByQueryKind::Delete => ("delete by query", Some("Deletes every document the query matches.")),
                };
                let mut info = StatementInfo::new(category(op.class()), kind, &excerpt).with_tables(vec![op.index.clone()]);
                info.reason = reason.map(str::to_owned);
                info
            }
            Err(error) => StatementInfo::refused(name, name, &format!("invalid arguments: {error}")),
        },
        _ => StatementInfo::refused(name, name, "unknown operation"),
    }
}

#[async_trait]
impl Engine for ElasticAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static ELASTICSEARCH_DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        static OPENSEARCH_DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        match self.flavor {
            Flavor::Elasticsearch => ELASTICSEARCH_DESCRIPTOR.get_or_init(descriptor::descriptor),
            Flavor::OpenSearch => OPENSEARCH_DESCRIPTOR.get_or_init(descriptor::opensearch_descriptor),
        }
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language, text } => self.classify_statement(language, text),
            ClassifyInput::Capability { name, args } => Classification::single(classify_capability(name, args)),
            _ => Classification::default(),
        }
    }

    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        self.parse(text)
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        Err(not_wired(self.flavor.id(), self.flavor.label()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use base64::{engine::general_purpose::STANDARD, Engine as _};

    use super::*;
    use crate::contract::{errors::ENGINE_NOT_WIRED, ConnectSecrets, Endpoint, EngineStatus};

    fn statement(engine: &ElasticAdapter, language: &str, text: &str) -> Classification {
        engine.classify(&ClassifyInput::Statement { language, text })
    }

    fn capability(name: &str, args: serde_json::Value) -> Classification {
        ELASTICSEARCH.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptors_are_planned_and_differ_by_flavor() {
        for (engine, id) in [(&ELASTICSEARCH, "elasticsearch"), (&OPENSEARCH, "opensearch")] {
            assert_eq!(engine.id(), id);
            assert_eq!(engine.descriptor().status, EngineStatus::Planned);
            assert_eq!(
                engine.descriptor().default_language().map(|language| language.id.as_str()),
                Some("es-console")
            );
        }
        let has_esql = |engine: &ElasticAdapter| engine.descriptor().languages.iter().any(|language| language.id == "esql");
        assert!(has_esql(&ELASTICSEARCH) && !has_esql(&OPENSEARCH));
        assert!(!OPENSEARCH.descriptor().connection.auth.contains(&AuthMethodId::ApiKey));
    }

    #[test]
    fn console_requests_are_classified_one_by_one() {
        let classification = statement(
            &ELASTICSEARCH,
            "es-console",
            "GET /products/_search\n{ \"query\": { \"match_all\": {} } }\n\nPOST /products/_doc\n{ \"name\": \"x\" }\n\nPUT /logs\n\nDELETE /products",
        );
        let categories: Vec<Category> = classification.statements.iter().map(|s| s.category).collect();
        assert_eq!(
            categories,
            vec![Category::Read, Category::Write, Category::Ddl, Category::Destructive]
        );
        assert_eq!(classification.tables(), vec!["products".to_owned(), "logs".to_owned()]);
        let broken = statement(&ELASTICSEARCH, "es-console", "{ not a request }");
        assert_eq!(broken.worst(), Some(Category::Unknown));
    }

    #[test]
    fn query_languages_follow_the_flavor() {
        assert_eq!(
            statement(&ELASTICSEARCH, "esql", "FROM logs | LIMIT 10").worst(),
            Some(Category::Read)
        );
        assert_eq!(statement(&OPENSEARCH, "esql", "FROM logs").worst(), Some(Category::Unknown));
        assert_eq!(
            statement(&ELASTICSEARCH, "es-sql", "SELECT * FROM logs").worst(),
            Some(Category::Read)
        );
        assert_eq!(statement(&OPENSEARCH, "es-sql", "DELETE FROM logs").worst(), Some(Category::Write));
    }

    #[test]
    fn engine_view_operations_are_classified_and_unknown_ones_refused() {
        assert_eq!(capability("es.clusterHealth", json!({})).worst(), Some(Category::Read));
        let delete = capability(
            "es.documentOp",
            json!({ "op": "delete", "index": "products", "id": "1", "options": {} }),
        );
        assert_eq!(delete.worst(), Some(Category::Write));
        assert_eq!(delete.tables(), vec!["products".to_owned()]);
        let by_query = |kind: &str| {
            capability(
                "es.byQuery",
                json!({ "kind": kind, "index": "products", "query": { "match_all": {} }, "proceedOnConflicts": false, "background": false, "refresh": false }),
            )
        };
        assert_eq!(by_query("update").worst(), Some(Category::Write));
        assert_eq!(by_query("delete").worst(), Some(Category::Destructive));
        assert!(capability("es.documentOp", json!({ "op": "nuke" })).refusal().is_some());
        assert_eq!(capability("es.reindexAll", json!({})).refusal(), Some("unknown operation"));
    }

    #[test]
    fn node_lists_split_into_a_spec_and_a_password() {
        let parsed = ELASTICSEARCH
            .parse_connection_string("https://elastic:s3cret@es1.local:9243, https://es2.local:9243")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.spec.engine, "elasticsearch");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("es1.local", 9243));
        assert_eq!(parsed.spec.user, "elastic");
        assert_eq!(parsed.spec.url.as_deref(), Some("https://es1.local:9243,https://es2.local:9243"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(!parsed.spec.url.as_deref().unwrap_or_default().contains("s3cret"));
        assert!(parsed.spec.options.values().all(|value| !value.contains("s3cret")));

        let single = OPENSEARCH.parse_connection_string("localhost:9200").unwrap();
        assert_eq!(single.spec.engine, "opensearch");
        assert_eq!((single.spec.host.as_str(), single.spec.port), ("localhost", 9200));
        assert!(single.spec.url.is_none() && single.spec.tls.is_none() && single.password.is_none());
        assert!(ELASTICSEARCH.parse_connection_string("ftp://es:21").is_err());
    }

    #[test]
    fn cloud_ids_become_an_option() {
        let cloud_id = format!("prod:{}", STANDARD.encode("eu-west-1.aws.found.io$abc123$kib456"));
        let parsed = ELASTICSEARCH.parse_connection_string(&cloud_id).unwrap();
        assert_eq!(parsed.spec.option("cloudId"), Some(cloud_id.as_str()));
        assert_eq!(
            (parsed.spec.host.as_str(), parsed.spec.port),
            ("abc123.eu-west-1.aws.found.io", 443)
        );
        assert!(parsed.password.is_none());
    }

    #[tokio::test]
    async fn connect_is_not_wired_yet() {
        for engine in [&ELASTICSEARCH, &OPENSEARCH] {
            let request = ConnectRequest {
                spec: ConnectionSpec::default(),
                secrets: ConnectSecrets::default(),
                endpoint: Endpoint {
                    host: "127.0.0.1".into(),
                    port: 1,
                },
                connect_timeout: Duration::from_secs(1),
            };
            let Err(error) = engine.connect(request).await else {
                panic!("a planned engine must not connect");
            };
            assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
            assert_eq!(error.payload().unwrap()["engine"], engine.id());
        }
    }
}
