//! Contract suites for Elasticsearch (`dev/compose/elasticsearch.yml`, https on 9200, elastic / studio-elastic, a
//! self-signed certificate) and OpenSearch (`dev/compose/opensearch.yml`, plain http on 9201, security plugin off).
//!
//! Before each suite the fixture recreates `contract_items` (1,000 documents, keys 1..=1000, the key declared in the
//! mapping as `_meta.fosta.key = "id"`) and `contract_children` through console requests. Plus engine checks: the
//! gateway (read-only workspace, refused paths, filter scripts), point-in-time paging past 10,000 hits on the seeded
//! `logs-app` data stream, edits through an alias, error mapping, and the tree.
//!
//! Override the servers with FOSTA_CONTRACT_ELASTICSEARCH_HOST / _PORT / _USER / _PASSWORD and
//! FOSTA_CONTRACT_OPENSEARCH_HOST / _PORT.

use std::sync::Arc;

use serde_json::json;

use crate::{
    contract::{
        testing::suites::*, BrowseRequest, CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Field, Filter, FilterCond,
        FilterOp, NodeKind, ObjectRef, Page, RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session, SessionOptions,
        SortKey, TlsConfig, TlsMode, TreeRequest, Value,
    },
    error::AppResult,
    gateway::{
        self,
        rules::{Preset, SafetySettings},
        Context,
    },
    runtime::{ConnectionManager, Handle},
};

fn env(engine: &str, name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_{engine}_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn item(i: i64) -> serde_json::Value {
    json!({
        "id": i,
        "name": format!("item {i:04}"),
        "price": (i as f64) * 1.25,
        "active": i % 2 == 0,
        "born": format!("2020-01-{:02}T00:00:00Z", (i % 28) + 1),
        "note": if i % 3 == 0 { serde_json::Value::Null } else { json!(format!("note {i}")) },
    })
}

/// Console requests that recreate the two indices.
fn seed() -> Vec<String> {
    let mut items = String::from("POST /contract_items/_bulk?refresh=true&filter_path=errors,took\n");
    for i in 1..=1000 {
        items.push_str(&format!("{}\n{}\n", json!({ "index": { "_id": i.to_string() } }), item(i)));
    }
    let mut children = String::from("POST /contract_children/_bulk?refresh=true&filter_path=errors,took\n");
    for i in 1..=200 {
        children.push_str(&format!(
            "{}\n{}\n",
            json!({ "index": { "_id": i.to_string() } }),
            json!({ "id": i, "item_id": i, "label": format!("child {i}") })
        ));
    }
    let items_mapping = json!({
        "settings": { "number_of_shards": 2, "number_of_replicas": 0 },
        "mappings": {
            "_meta": { "fosta": { "key": "id" } },
            "properties": {
                "id": { "type": "long" },
                "name": { "type": "keyword" },
                "price": { "type": "scaled_float", "scaling_factor": 100 },
                "active": { "type": "boolean" },
                "born": { "type": "date" },
                "note": { "type": "keyword" }
            }
        }
    });
    let children_mapping = json!({
        "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
        "mappings": {
            "_meta": { "fosta": { "key": "id" } },
            "properties": { "id": { "type": "long" }, "item_id": { "type": "long" }, "label": { "type": "keyword" } }
        }
    });
    vec![
        "DELETE /contract_items?ignore_unavailable=true\n\nDELETE /contract_children?ignore_unavailable=true".to_owned(),
        format!("PUT /contract_items\n{items_mapping}\n\nPUT /contract_children\n{children_mapping}"),
        items,
        children,
    ]
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::EsQueryDsl,
        text: json!({ "term": { "id": key } }).to_string(),
    }
}

fn elastic_spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "elasticsearch".into(),
        host: env("ELASTICSEARCH", "HOST", "127.0.0.1"),
        port: env("ELASTICSEARCH", "PORT", "9200").parse().unwrap_or(9200),
        user: env("ELASTICSEARCH", "USER", "elastic"),
        // The dev cluster's certificate is self-signed.
        tls: Some(TlsConfig {
            mode: TlsMode::Require,
            accept_invalid_certs: true,
            ..TlsConfig::default()
        }),
        ..ConnectionSpec::default()
    }
}

fn elastic_secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("ELASTICSEARCH", "PASSWORD", "studio-elastic"))
}

fn opensearch_spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "opensearch".into(),
        host: env("OPENSEARCH", "HOST", "127.0.0.1"),
        port: env("OPENSEARCH", "PORT", "9201").parse().unwrap_or(9201),
        ..ConnectionSpec::default()
    }
}

const CLASSIFY_SAMPLES: &[(&str, Category)] = &[
    ("GET /contract_items/_search", Category::Read),
    ("POST /contract_items/_count\n{ \"query\": { \"match_all\": {} } }", Category::Read),
    ("PUT /contract_items/_doc/1\n{ \"name\": \"x\" }", Category::Write),
    ("POST /_bulk\n{ \"index\": { \"_index\": \"t\" } }\n{ \"a\": 1 }", Category::Write),
    ("PUT /contract_items/_mapping\n{ \"properties\": {} }", Category::Ddl),
    ("DELETE /contract_items", Category::Destructive),
    (
        "POST /contract_items/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
        Category::Destructive,
    ),
    ("POST /contract_items/_update_by_query", Category::Destructive),
    ("PUT /_cluster/settings\n{ \"persistent\": {} }", Category::Admin),
];

fn fixture_for(engine: &'static str, spec: ConnectionSpec, secrets: ConnectSecrets) -> Fixture {
    Fixture {
        engine,
        spec,
        secrets,
        table: ObjectRef::new(None, None, "contract_items", NodeKind::COLLECTION),
        child: ObjectRef::new(None, None, "contract_children", NodeKind::COLLECTION),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("GET /_cluster/health?wait_for_nodes=99&timeout=30s"),
        syntax_error: "GET /contract_items/_search\n{ \"query\": { \"matc\": {} } }",
        constraint_violation: "PUT /contract_items/_create/5\n{ \"id\": 5, \"name\": \"dup\" }",
        multi_statement: "GET /contract_items/_count\n\nGET /contract_items/_search\n{ \"size\": 3 }",
        insert_probe: "PUT /contract_items/_doc/5000?refresh=true\n{ \"id\": 5000, \"name\": \"probe\" }",
        count_probe: "GET /contract_items/_count\n{ \"query\": { \"term\": { \"id\": 5000 } } }",
        classify_samples: CLASSIFY_SAMPLES,
        language: "es-console",
        gaps: &[],
    }
}

pub fn fixture() -> Fixture {
    fixture_for("elasticsearch", elastic_spec(), elastic_secrets())
}

pub fn opensearch_fixture() -> Fixture {
    fixture_for("opensearch", opensearch_spec(), ConnectSecrets::default())
}

crate::contract_tests!(contract_elasticsearch, fixture());
crate::contract_tests!(contract_opensearch, opensearch_fixture());

// --- Engine checks -------------------------------------------------------------------------------------------------

async fn session(handle: &Handle, language: Option<&str>) -> Arc<dyn Session> {
    let options = SessionOptions {
        language: language.map(str::to_owned),
        database: None,
    };
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&options, &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.into(),
        database: None,
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
    Ok(sink)
}

async fn browse(handle: &Handle, request: BrowseRequest) -> AppResult<Page> {
    handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&request, &CallContext::default()).await
        })
        .await
}

fn context(connection: &str, preset: Preset) -> Context {
    Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: connection.into(),
        connection_name: connection.into(),
        safety: SafetySettings::preset(preset),
    }
}

async fn gateway_checks(f: Fixture, connection: &str) {
    let (manager, _handle) = connect(&f).await;
    // `connect` seeds under its own id; connect again under ours for the gateway.
    manager
        .connect(connection, f.spec.clone(), f.secrets.clone())
        .await
        .expect("connect");
    manager
        .open_session(connection, format!("{connection}-s"), SessionOptions::default())
        .await
        .expect("session");
    let session_id = format!("{connection}-s");
    let read_only = context(connection, Preset::ReadOnly);
    let request = |text: &str| RunRequest {
        text: text.into(),
        database: None,
        offset: 0,
    };
    let mut sink = CollectSink::default();
    for text in [
        "DELETE /contract_items",
        "POST /contract_items/_delete_by_query\n{ \"query\": { \"match_all\": {} } }",
        "PUT /contract_items/_doc/1\n{ \"name\": \"x\" }",
        "GET /contract_items/_search\n\nPUT /_cluster/settings\n{ \"transient\": {} }",
    ] {
        let error = gateway::run(&manager, &read_only, &session_id, &request(text), None, &mut sink)
            .await
            .expect_err(text);
        assert_eq!(error.code(), Some(gateway::BLOCKED), "{text}: {error}");
    }
    // A path the client would rewrite is refused even in an open workspace, and nothing reaches the cluster.
    let open = context(connection, Preset::Open);
    for text in [
        "DELETE /_count/../contract_items",
        "DELETE /contract_items#/_search",
        "DELETE /_search/%2e%2e/contract_items",
    ] {
        let error = gateway::run(&manager, &open, &session_id, &request(text), None, &mut sink)
            .await
            .expect_err(text);
        assert!(error.message().starts_with("Refused"), "{text}: {error}");
    }
    gateway::run(
        &manager,
        &read_only,
        &session_id,
        &request("GET /contract_items/_count"),
        None,
        &mut sink,
    )
    .await
    .expect("reads run in a read-only workspace");
    // Raw filters must not run scripts or smuggle them in a wrapper query, whatever the workspace.
    for text in [
        r#"{ "script": { "script": "Thread.sleep(1000); return true" } }"#,
        r#"{ "wrapper": { "query": "eyJzY3JpcHQiOnsic2NyaXB0IjoidHJ1ZSJ9fQ==" } }"#,
        r#"{ "terms_set": { "name": { "terms": ["item 0001"], "minimum_should_match_script": { "source": "1" } } } }"#,
        r#"{ "function_score": { "query": { "match_all": {} }, "script_score": { "script": "1" } } }"#,
    ] {
        let mut browse_request = BrowseRequest::new(f.table.clone(), 5);
        browse_request.filter = Filter::Raw {
            language: RawFilterLang::EsQueryDsl,
            text: text.into(),
        };
        let error = gateway::browse(&manager, &open, connection, &browse_request).await.expect_err(text);
        assert_eq!(error.code(), Some("INVALID_FILTER"), "{text}: {error}");
        // The adapter checks again when browse is reached without the gateway.
        let direct = browse(&manager.handle(connection).unwrap(), browse_request).await.expect_err(text);
        assert_eq!(direct.code(), Some("INVALID_FILTER"), "{text}: {direct}");
    }
    // The index is still there.
    let page = browse(&manager.handle(connection).unwrap(), BrowseRequest::new(f.table.clone(), 1))
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    manager.close_session(&session_id).await;
    manager.disconnect(connection).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/elasticsearch.yml"]
async fn contract_elasticsearch_gateway() {
    gateway_checks(fixture(), "gate-es").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/opensearch.yml"]
async fn contract_opensearch_gateway() {
    gateway_checks(opensearch_fixture(), "gate-os").await;
}

/// Errors carry the error type as vendor code, the cause chain, the HTTP status and a position in the editor.
async fn error_checks(f: Fixture) {
    let (_manager, handle) = connect(&f).await;
    let console = session(&handle, None).await;
    let text = "GET /contract_items/_count\n\nGET /contract_items/_search\n{ \"query\": { \"matc\": {} } }";
    let error = run(&handle, &console, text).await.expect_err("parsing error");
    assert_eq!(error.code(), Some("ENGINE_ERROR"));
    assert_eq!(error.field("vendorCode").and_then(|v| v.as_str()), Some("parsing_exception"));
    assert_eq!(error.field("status").and_then(|v| v.as_u64()), Some(400));
    assert_eq!(error.field("statement").unwrap()["index"], 1);
    assert_eq!(error.field("position").unwrap()["line"], 4, "{error:?}");
    let cause = error.field("cause").and_then(|c| c.as_array()).expect("cause chain");
    assert!(!cause.is_empty());
    let mapping_error = run(
        &handle,
        &console,
        "GET /contract_items/_search\n{ \"query\": { \"range\": { \"id\": { \"gte\": \"x\" } } } }",
    )
    .await
    .expect_err("bad number");
    assert!(mapping_error.field("cause").is_some(), "{mapping_error:?}");
    let missing = run(&handle, &console, "GET /no_such_index_fosta/_search")
        .await
        .expect_err("missing index");
    assert_eq!(
        missing.field("vendorCode").and_then(|v| v.as_str()),
        Some("index_not_found_exception")
    );
    assert_eq!(missing.field("status").and_then(|v| v.as_u64()), Some(404));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/elasticsearch.yml"]
async fn contract_elasticsearch_errors() {
    error_checks(fixture()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/opensearch.yml"]
async fn contract_opensearch_errors() {
    error_checks(opensearch_fixture()).await;
}

/// The seeded `logs-app` data stream (20,000 documents on Elasticsearch, 2,000 on OpenSearch; `dev/seed-nosql.sh`) pages
/// by keyset through all of it, with `_index` in the key and no repeats.
async fn data_stream_paging(f: Fixture, expected: usize) {
    let (_manager, handle) = connect(&f).await;
    let top = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    let streams = top
        .nodes
        .iter()
        .find(|node| node.label == "Data streams")
        .expect("data streams folder");
    let request = TreeRequest {
        parent: Some(streams.key.clone()),
        ..TreeRequest::default()
    };
    let children = handle
        .call(
            "listed",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .unwrap();
    let Some(stream) = children.nodes.iter().find(|node| node.label == "logs-app") else {
        println!("logs-app is not seeded (sh dev/seed-nosql.sh); skipped");
        return;
    };
    assert!(stream.kind.is_relation());
    let object = stream.object.clone().unwrap();
    let mut request = BrowseRequest::new(object, 1000);
    request.estimate_total = true;
    let mut seen = std::collections::HashSet::new();
    let mut pages = 0;
    loop {
        let page = browse(&handle, request.clone()).await.expect("page");
        assert_eq!(page.key_columns, vec!["_index".to_owned(), "_id".to_owned()]);
        for row in &page.rows {
            let id = format!("{}/{}", row[1].display_or_null(), row[0].display_or_null());
            assert!(seen.insert(id), "a document came twice");
        }
        pages += 1;
        match page.next_keyset {
            Some(after) if page.has_more => request.after = Some(after),
            _ => break,
        }
        assert!(pages < 100);
    }
    assert_eq!(seen.len(), expected);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/elasticsearch.yml"]
async fn contract_elasticsearch_point_in_time_paging() {
    data_stream_paging(fixture(), 20_000).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/opensearch.yml"]
async fn contract_opensearch_point_in_time_paging() {
    data_stream_paging(opensearch_fixture(), 2_000).await;
}

/// Through an alias the key includes `_index` and edits go to the document's own index; `_id` cannot be ranged.
async fn alias_checks(f: Fixture) {
    let (_manager, handle) = connect(&f).await;
    let console = session(&handle, None).await;
    run(
        &handle,
        &console,
        "POST /_aliases\n{ \"actions\": [ { \"remove\": { \"index\": \"*\", \"alias\": \"contract_alias\", \"must_exist\": false } }, { \"add\": { \"index\": \"contract_items\", \"alias\": \"contract_alias\", \"is_write_index\": true } } ] }",
    )
    .await
    .expect("alias");
    let alias = ObjectRef::new(None, None, "contract_alias", NodeKind::COLLECTION);
    let mut request = BrowseRequest::new(alias.clone(), 5);
    request.sort = vec![SortKey::asc("id")];
    let page = browse(&handle, request).await.expect("alias page");
    assert_eq!(page.key_columns, vec!["_index".to_owned(), "id".to_owned()]);
    let versions = page.row_versions.clone().unwrap();
    let change = RowChange::Update {
        key: vec![
            Field::new("_index", Value::Text("contract_items".into())),
            Field::new("id", Value::Int(1)),
        ],
        values: vec![Field::new("name", Value::Text("through the alias".into()))],
        original: vec![Field::new("name", Value::Text("item 0001".into()))],
        version: Some(versions[0].clone()),
        force: false,
    };
    let result = handle
        .call("applied", |conn| async move {
            conn.tabular()
                .unwrap()
                .apply(
                    &RowChangeSet {
                        object: alias,
                        changes: vec![change],
                    },
                    &CallContext::default(),
                )
                .await
        })
        .await
        .expect("edit through the alias");
    assert_eq!(result.updated, 1);
    let mut by_id = BrowseRequest::new(f.table.clone(), 5);
    by_id.filter = Filter::Conditions {
        conditions: vec![FilterCond::new("_id", FilterOp::Eq, &["1"])],
        match_any: false,
    };
    let page = browse(&handle, by_id.clone()).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    let name = page.columns.iter().position(|c| c.name == "name").unwrap();
    assert_eq!(page.rows[0][name], Value::Text("through the alias".into()));
    by_id.filter = Filter::Conditions {
        conditions: vec![FilterCond::new("_id", FilterOp::Lt, &["5"])],
        match_any: false,
    };
    assert!(browse(&handle, by_id).await.is_err(), "_id cannot be ranged");
    // Describe shows the mapping.
    let description = handle
        .call("described", |conn| async move {
            conn.describe(
                &ObjectRef::new(None, None, "contract_items", NodeKind::COLLECTION),
                &CallContext::default(),
            )
            .await
        })
        .await
        .unwrap();
    assert!(description.ddl.as_deref().unwrap_or_default().contains("\"scaled_float\""));
    assert_eq!(description.row_estimate, Some(1000));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/elasticsearch.yml"]
async fn contract_elasticsearch_alias_edits() {
    alias_checks(fixture()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/opensearch.yml"]
async fn contract_opensearch_alias_edits() {
    alias_checks(opensearch_fixture()).await;
}

/// An Elasticsearch profile on the OpenSearch server is refused (and the reverse), and ES|QL and SQL sessions stream
/// typed tables.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/elasticsearch.yml and dev/compose/opensearch.yml"]
async fn contract_elasticsearch_flavors_and_languages() {
    let wrong = ConnectionSpec {
        engine: "elasticsearch".into(),
        ..opensearch_spec()
    };
    let error = ConnectionManager::probe(wrong, ConnectSecrets::default())
        .await
        .expect_err("OpenSearch refused");
    assert!(error.message().contains("OpenSearch engine"), "{error}");
    let reverse = ConnectionSpec {
        engine: "opensearch".into(),
        ..elastic_spec()
    };
    let error = ConnectionManager::probe(reverse, elastic_secrets())
        .await
        .expect_err("Elasticsearch refused");
    assert!(error.message().contains("Elasticsearch engine"), "{error}");

    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let info = manager.connect_info(CONNECTION).unwrap();
    assert!(info.languages.iter().any(|language| language.id == "esql"));
    let esql = session(&handle, Some("esql")).await;
    let sink = run(
        &handle,
        &esql,
        "FROM contract_items | WHERE id <= 5 | SORT id | KEEP id, name, price",
    )
    .await
    .expect("esql");
    assert_eq!(sink.sets[0].rows.len(), 5);
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(1));
    let error = run(&handle, &esql, "FROM contract_items | WHERE nope > 1")
        .await
        .expect_err("unknown column");
    assert!(error.field("position").is_some(), "{error:?}");
    let sql = session(&handle, Some("es-sql")).await;
    let sink = run(&handle, &sql, "SELECT id, name FROM contract_items ORDER BY id LIMIT 3")
        .await
        .expect("sql");
    assert_eq!(sink.sets[0].rows.len(), 3);

    let (os_manager, os_handle) = connect(&opensearch_fixture()).await;
    let info = os_manager.connect_info(CONNECTION).unwrap();
    assert!(!info.languages.iter().any(|language| language.id == "esql"));
    let os_sql = session(&os_handle, Some("es-sql")).await;
    let sink = run(&os_handle, &os_sql, "SELECT id, name FROM contract_items ORDER BY id LIMIT 3")
        .await
        .expect("OpenSearch SQL");
    assert_eq!(sink.sets[0].rows.len(), 3);
}
