//! The Elasticsearch and OpenSearch engine ports: one adapter type, two statics with their own descriptors. A
//! connection is an [`EsEngine`] (a stateless HTTP client with node failover); the tree, the document grid, the
//! sessions and the engine operations all use it.
//!
//! Engine views call these `invoke` names: `es.indexDetails`, `es.clusterHealth`, `es.cat`, `es.document`, `es.names`
//! and `es.fields` (reads), `es.documentOp` (a [`DocumentOp`]) and `es.byQuery` (a [`ByQueryOp`]), both classified by
//! their own `class()`, and the vector operations (`vector.*`, reads; see [`super::vector`]). Unknown names are refused.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::json;

use super::{
    admin::CatApi,
    classify::{classify, RequestClass, RequestClassification},
    client::EsBody,
    config::{parse_cloud_id, EsConnectOptions},
    connect,
    connection::EsEngine,
    console,
    descriptor::{self, Flavor, DEFAULT_PORT},
    docs::{ByQueryKind, ByQueryOp, DocumentOp},
    dsl, edits, grid, profile,
    session::{self, CONSOLE, ESQL, SQL},
    tree, vector,
};
use crate::{
    contract::{
        classify_common, default_validate_filter, require, unsupported_capability, value::percent_decode,
        vector::OPERATIONS as VECTOR_OPERATIONS, ApplyResult, AuthMethodId, BrowseRequest, CallContext, Capability, CapabilitySet,
        Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine, EngineDescriptor, Filter,
        LanguageSpec, NodeAction, ObjectDescription, ObjectRef, OpenedSession, Page, ParsedConnection, ProfileEnrichment, ProfileExtras,
        RawFilterLang, RowChangeSet, ServerInfo, SessionOptions, StatementInfo, TabularData, TlsConfig, TlsMode, TreeChildren, TreeRequest,
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
            CONSOLE => classify_console(text),
            ESQL if self.flavor == Flavor::Elasticsearch => Classification::single(request_info(
                classify(&Method::POST, "/_query", Some(&json!({ "query": text }))),
                text,
            )),
            SQL => Classification::single(request_info(
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
    if let Some(reason) = &classification.refused {
        return StatementInfo::refused(&classification.api, excerpt, reason);
    }
    let mut info = StatementInfo::new(category(classification.class), &classification.api, excerpt)
        .with_tables(classification.target.into_iter().collect());
    info.reason = classification.reason;
    info
}

/// Console text: each request is classified from its method, path and body.
pub(super) fn classify_console(text: &str) -> Classification {
    match console::parse(text) {
        Ok(requests) if requests.is_empty() => Classification::single(StatementInfo::new(Category::Read, "empty", text)),
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

const READ_OPERATIONS: [&str; 6] = [
    "es.indexDetails",
    "es.clusterHealth",
    "es.cat",
    "es.document",
    "es.names",
    "es.fields",
];

fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    let excerpt = format!("{name} {args}");
    match name {
        _ if READ_OPERATIONS.contains(&name) => {
            let index = args.get("index").and_then(|index| index.as_str()).map(str::to_owned);
            StatementInfo::new(Category::Read, name, &excerpt).with_tables(index.into_iter().collect())
        }
        _ if VECTOR_OPERATIONS.contains(&name) => {
            let index = args.get("table").and_then(|table| table.as_str()).map(str::to_owned);
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
                    ByQueryKind::Update => ("update by query", "Changes every document the query matches."),
                    ByQueryKind::Delete => ("delete by query", "Deletes every document the query matches."),
                };
                StatementInfo::new(category(op.class()), kind, &excerpt)
                    .with_tables(vec![op.index.clone()])
                    .with_reason(reason)
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

    /// The default checks plus the strict Query DSL check of [`dsl::parse_filter`] (no scripts, no `wrapper`).
    fn validate_filter(&self, filter: &Filter) -> AppResult<()> {
        default_validate_filter(self.descriptor(), filter)?;
        match filter {
            Filter::Raw {
                language: RawFilterLang::EsQueryDsl,
                text,
            } if !text.trim().is_empty() => dsl::parse_filter(text).map(|_| ()),
            _ => Ok(()),
        }
    }

    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        self.parse(text)
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let engine = connect::connect(&request, self.flavor).await?;
        let features = engine.features();
        // Without a point in time (ES before 7.10, OpenSearch before 2.4) keyset paging still works on a declared key
        // and on sets within the first 10,000 documents, so the capabilities stay those of the descriptor.
        let mut capabilities = self.descriptor().capabilities;
        if !vector::cluster_supports(&engine).await {
            capabilities = capabilities.without(Capability::Vector);
        }
        let languages = self
            .descriptor()
            .languages
            .iter()
            .filter(|language| match language.id.as_str() {
                ESQL => features.esql,
                SQL => features.sql,
                _ => true,
            })
            .cloned()
            .collect();
        Ok(Arc::new(EsConnection {
            info: connect::server_info(&engine, self.flavor),
            engine,
            flavor: self.flavor,
            capabilities,
            languages,
        }))
    }
}

/// An open cluster.
pub struct EsConnection {
    engine: EsEngine,
    flavor: Flavor,
    info: ServerInfo,
    capabilities: CapabilitySet,
    languages: Vec<LanguageSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexArgs {
    #[serde(default)]
    index: String,
    #[serde(default)]
    include_defaults: bool,
    #[serde(default)]
    id: String,
    #[serde(default)]
    routing: Option<String>,
}

#[async_trait]
impl Connection for EsConnection {
    fn engine_id(&self) -> &str {
        self.flavor.id()
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    fn languages(&self) -> Vec<LanguageSpec> {
        self.languages.clone()
    }

    fn connection_actions(&self) -> Vec<NodeAction> {
        vec![
            NodeAction::query("health", "Cluster health", "GET /_cluster/health"),
            NodeAction::query("indices", "List indices", "GET /_cat/indices?v&s=index"),
        ]
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        tree::children(&self.engine, request).await
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        tree::describe(&self.engine, self.flavor, object).await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        let language = options.language.as_deref().unwrap_or(CONSOLE);
        if !self.languages.iter().any(|spec| spec.id == language) {
            return Err(AppError::new(format!(
                "This {} connection does not run the '{language}' language.",
                self.flavor.label()
            )));
        }
        let (session, canceller) = session::open(self.engine.clone(), language);
        Ok(OpenedSession {
            session,
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        let index_args =
            || serde_json::from_value::<IndexArgs>(args.clone()).map_err(|error| AppError::new(format!("invalid arguments: {error}")));
        match name {
            "es.clusterHealth" => Ok(serde_json::to_value(self.engine.health().await?)?),
            "es.indexDetails" => {
                let args = index_args()?;
                Ok(serde_json::to_value(
                    self.engine.index_details(&args.index, args.include_defaults).await?,
                )?)
            }
            "es.document" => {
                let args = index_args()?;
                Ok(serde_json::to_value(
                    self.engine.get_document(&args.index, &args.id, args.routing.as_deref()).await?,
                )?)
            }
            "es.cat" => {
                let api: CatApi = serde_json::from_value(args.get("api").cloned().unwrap_or_default())
                    .map_err(|error| AppError::new(format!("invalid arguments: {error}")))?;
                Ok(serde_json::to_value(self.engine.cat(api).await?)?)
            }
            "es.names" => {
                let (indices, aliases, streams) = futures_util::join!(
                    self.engine.list_indices(false),
                    self.engine.list_aliases(false),
                    self.engine.list_data_streams(false)
                );
                let mut names: Vec<String> = indices?.into_iter().map(|index| index.name).collect();
                names.extend(aliases.unwrap_or_default().into_iter().map(|alias| alias.alias));
                names.extend(streams.unwrap_or_default().into_iter().map(|stream| stream.name));
                names.sort();
                names.dedup();
                Ok(json!({ "names": names }))
            }
            "es.fields" => {
                let args = index_args()?;
                let target = grid::resolve(&self.engine, &args.index).await?;
                Ok(json!({
                    "fields": target.columns.iter().map(|column| json!({ "name": column.name, "type": column.field_type })).collect::<Vec<_>>()
                }))
            }
            "es.documentOp" => {
                let op: DocumentOp = serde_json::from_value(args).map_err(|error| AppError::new(format!("invalid arguments: {error}")))?;
                Ok(serde_json::to_value(self.engine.apply_document_op(&op).await?)?)
            }
            "es.byQuery" => {
                let op: ByQueryOp = serde_json::from_value(args).map_err(|error| AppError::new(format!("invalid arguments: {error}")))?;
                Ok(serde_json::to_value(self.engine.by_query(&op).await?)?)
            }
            _ if VECTOR_OPERATIONS.contains(&name) => {
                require(self.capabilities, Capability::Vector)?;
                vector::invoke(&self.engine, name, args).await
            }
            _ => Err(unsupported_capability(name)),
        }
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn profile_extras(&self) -> Option<&dyn ProfileExtras> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.engine.client().get("/").await.map(|_| ())
    }

    async fn close(&self) {}
}

#[async_trait]
impl ProfileExtras for EsConnection {
    async fn metadata(&self, _database: &str, objects: &[ObjectRef], _ctx: &CallContext) -> AppResult<ProfileEnrichment> {
        profile::metadata(&self.engine, objects).await
    }
}

#[async_trait]
impl TabularData for EsConnection {
    async fn browse(&self, request: &BrowseRequest, _ctx: &CallContext) -> AppResult<Page> {
        if request.reverse {
            return Err(unsupported_capability("reading an index from its end"));
        }
        grid::browse(&self.engine, request).await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        if !self.capabilities.contains(Capability::EditRows) {
            return Err(unsupported_capability("editing rows"));
        }
        edits::apply(&self.engine, changes).await
    }
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    use super::*;
    use crate::contract::EngineStatus;

    fn statement(engine: &ElasticAdapter, language: &str, text: &str) -> Classification {
        engine.classify(&ClassifyInput::Statement { language, text })
    }

    fn capability(name: &str, args: serde_json::Value) -> Classification {
        ELASTICSEARCH.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptors_are_ready_and_differ_by_flavor() {
        for (engine, id) in [(&ELASTICSEARCH, "elasticsearch"), (&OPENSEARCH, "opensearch")] {
            assert_eq!(engine.id(), id);
            assert_eq!(engine.descriptor().status, EngineStatus::Ready);
            assert_eq!(
                engine.descriptor().default_language().map(|language| language.id.as_str()),
                Some("es-console")
            );
            let capabilities = engine.descriptor().capabilities;
            for capability in [
                Capability::KeysetPaging,
                Capability::Cancel,
                Capability::EditRows,
                Capability::RawFilter,
            ] {
                assert!(capabilities.contains(capability), "{id}: {capability:?}");
            }
            assert!(!capabilities.contains(Capability::Transactions));
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
    fn rewritten_paths_are_refused_by_the_gateway() {
        for text in [
            "DELETE /_count/../products",
            "POST /_search/../products/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
            "DELETE /products#/_search",
            "GET /products/_search\n\nDELETE /_search/%2e%2e/products",
        ] {
            let classification = statement(&ELASTICSEARCH, "es-console", text);
            assert!(classification.refusal().is_some(), "{text}");
        }
        assert!(statement(&OPENSEARCH, "es-console", "GET /products/_search").refusal().is_none());
    }

    #[test]
    fn the_spec_requests_have_their_classes() {
        let worst = |text: &str| statement(&ELASTICSEARCH, "es-console", text).worst();
        for read in [
            "GET /products/_search",
            "POST /products/_search\n{}",
            "GET /products/_count",
            "GET /products/_mapping",
        ] {
            assert_eq!(worst(read), Some(Category::Read), "{read}");
        }
        for write in [
            "PUT /products/_doc/1\n{}",
            "POST /products/_doc\n{}",
            "POST /products/_update/1\n{ \"doc\": {} }",
            "POST /_bulk\n{ \"delete\": { \"_index\": \"p\", \"_id\": \"1\" } }",
        ] {
            assert_eq!(worst(write), Some(Category::Write), "{write}");
        }
        for dangerous in [
            "DELETE /products",
            "POST /products/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
            "POST /products/_update_by_query",
            "PUT /_cluster/settings\n{ \"persistent\": {} }",
        ] {
            assert!(
                matches!(worst(dangerous), Some(Category::Destructive | Category::Admin)),
                "{dangerous}"
            );
        }
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
        assert_eq!(capability("es.names", json!({})).worst(), Some(Category::Read));
        assert_eq!(capability("es.fields", json!({ "index": "p" })).tables(), vec!["p".to_owned()]);
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
        assert_eq!(by_query("update").worst(), Some(Category::Destructive));
        assert_eq!(by_query("delete").worst(), Some(Category::Destructive));
        assert!(capability("es.documentOp", json!({ "op": "nuke" })).refusal().is_some());
        assert_eq!(capability("es.reindexAll", json!({})).refusal(), Some("unknown operation"));
        for name in VECTOR_OPERATIONS {
            let vector = capability(name, json!({ "database": "", "table": "products", "column": "embedding" }));
            assert_eq!(vector.worst(), Some(Category::Read), "{name}");
            assert_eq!(vector.tables(), vec!["products".to_owned()]);
        }
        assert!(ELASTICSEARCH.descriptor().capabilities.contains(Capability::Vector));
        assert!(OPENSEARCH.descriptor().capabilities.contains(Capability::Vector));
    }

    #[test]
    fn raw_filters_are_checked_by_the_engine_too() {
        let raw = |text: &str| Filter::Raw {
            language: RawFilterLang::EsQueryDsl,
            text: text.to_owned(),
        };
        assert!(ELASTICSEARCH.validate_filter(&raw(r#"{ "term": { "status": "paid" } }"#)).is_ok());
        for bad in [
            r#"{ "wrapper": { "query": "eyJzY3JpcHQiOnt9fQ==" } }"#,
            r#"{ "terms_set": { "t": { "terms": ["a"], "minimum_should_match_script": { "source": "1" } } } }"#,
            r#"{ "script": { "script": "1" } }"#,
        ] {
            assert_eq!(
                ELASTICSEARCH.validate_filter(&raw(bad)).unwrap_err().code(),
                Some(dsl::INVALID_FILTER),
                "{bad}"
            );
            assert!(OPENSEARCH.validate_filter(&raw(bad)).is_err());
        }
        let sql = Filter::Raw {
            language: RawFilterLang::Sql,
            text: "1=1".into(),
        };
        assert!(ELASTICSEARCH.validate_filter(&sql).is_err());
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
}
