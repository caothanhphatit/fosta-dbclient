//! Live tests against `dev/docker-compose.engines-nosql.yml` after `sh dev/seed-nosql.sh`:
//! Elasticsearch 8 on https://127.0.0.1:9200 (elastic / studio-elastic, generated certificates) and, when it runs,
//! OpenSearch 2 on http://127.0.0.1:9201. Run with `cargo test live_engine_ -- --ignored --test-threads=1`.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;

use super::{
    admin::{AnalyzeRequest, CatApi, IndexOp, PipelineRef},
    catalog::CatalogOptions,
    classify::RequestClass,
    config::{EsAuth, EsConnectOptions},
    connection::{Distribution, EsEngine},
    docs::{ByQueryKind, ByQueryOp, DocumentOp, WriteOptions},
    mapping::FieldCategory,
    query_lang::{EsqlRequest, SqlRequest},
    search::{AggResult, Paging, SearchRequest},
};

const ES_URL: &str = "https://127.0.0.1:9200";
const OS_URL: &str = "http://127.0.0.1:9201";
const TEST_INDEX: &str = "fosta-test-docs";

fn es_options() -> EsConnectOptions {
    let mut options = EsConnectOptions::single(ES_URL);
    options.auth = EsAuth::Basic {
        username: "elastic".into(),
        password: Some("studio-elastic".into()),
    };
    options
}

async fn es() -> EsEngine {
    let mut options = es_options();
    options.tls.accept_invalid_certs = true;
    EsEngine::connect(&options)
        .await
        .expect("Elasticsearch from dev/docker-compose.engines-nosql.yml must be running")
}

/// The cluster's generated CA, read from the container.
fn ca_pem() -> Option<String> {
    let output = std::process::Command::new("docker")
        .args([
            "exec",
            "fosta-dev-nosql-elasticsearch-1",
            "cat",
            "/usr/share/elasticsearch/config/certs/http_ca.crt",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn pem_sha256(pem: &str) -> String {
    let body: String = pem.lines().filter(|line| !line.starts_with("-----")).collect();
    let der = STANDARD.decode(body).unwrap();
    ring::digest::digest(&ring::digest::SHA256, &der)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_connect_and_tls() {
    let engine = es().await;
    assert_eq!(engine.info().distribution, Distribution::Elasticsearch);
    assert_eq!(engine.info().major, 8);
    assert!(engine.features().esql);
    let health = engine.health().await.unwrap();
    assert!(health.status == "green" || health.status == "yellow", "{}", health.status);

    // Without trusting the generated CA the handshake fails with a TLS hint.
    let error = EsEngine::connect(&es_options())
        .await
        .err()
        .expect("self-signed must be refused")
        .to_string();
    assert!(error.contains("TLS") || error.contains("certificate"), "{error}");

    // Wrong password.
    let mut wrong = es_options();
    wrong.tls.accept_invalid_certs = true;
    wrong.auth = EsAuth::Basic {
        username: "elastic".into(),
        password: Some("nope".into()),
    };
    let error = EsEngine::connect(&wrong).await.err().unwrap().to_string();
    assert!(error.contains("401"), "{error}");

    let Some(pem) = ca_pem() else {
        eprintln!("docker exec not available: CA and fingerprint checks skipped");
        return;
    };
    let mut with_ca = es_options();
    with_ca.tls.ca_pem = Some(pem.clone());
    EsEngine::connect(&with_ca).await.expect("trusting the generated CA");

    let mut pinned = es_options();
    pinned.tls.ca_fingerprint = Some(pem_sha256(&pem));
    EsEngine::connect(&pinned).await.expect("pinning the CA fingerprint");
    pinned.tls.ca_fingerprint = Some("00".repeat(32));
    assert!(EsEngine::connect(&pinned).await.is_err(), "a wrong fingerprint must be refused");
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_catalog_and_mappings() {
    let engine = es().await;
    let catalog = engine.catalog(&CatalogOptions::default()).await;
    assert!(catalog.indices.error.is_none(), "{:?}", catalog.indices.error);
    let products = catalog
        .indices
        .items
        .iter()
        .find(|index| index.name == "products")
        .expect("seeded products index");
    assert_eq!(products.docs_count, Some(300));
    assert!(products.store_bytes.unwrap_or(0) > 0);
    assert!(
        !catalog.indices.items.iter().any(|index| index.hidden),
        "hidden indices only with include_hidden"
    );
    let stream = catalog
        .data_streams
        .items
        .iter()
        .find(|stream| stream.name == "logs-app")
        .expect("logs-app data stream");
    assert!(!stream.backing_indices.is_empty());
    assert_eq!(stream.template.as_deref(), Some("logs-template"));
    assert!(catalog
        .aliases
        .items
        .iter()
        .any(|alias| alias.alias == "products-current" && alias.index == "products"));
    let template = catalog
        .index_templates
        .items
        .iter()
        .find(|template| template.name == "logs-template")
        .unwrap();
    assert!(template.data_stream);
    assert_eq!(template.composed_of, vec!["logs-mappings"]);
    assert!(catalog
        .component_templates
        .items
        .iter()
        .any(|template| template.name == "logs-mappings"));
    let pipeline = catalog
        .pipelines
        .items
        .iter()
        .find(|pipeline| pipeline.id == "logs-pipeline")
        .unwrap();
    assert_eq!(pipeline.processors, vec!["lowercase", "rename", "set"]);
    let policy = catalog
        .lifecycle_policies
        .items
        .iter()
        .find(|policy| policy.name == "logs-policy")
        .unwrap();
    assert!(policy.stages.contains(&"hot".to_string()) && policy.stages.contains(&"delete".to_string()));
    assert!(catalog.snapshot_repositories.error.is_none());

    let hidden = engine.catalog(&CatalogOptions { include_hidden: true }).await;
    let backing = hidden
        .indices
        .items
        .iter()
        .find(|index| index.name.starts_with(".ds-logs-app"))
        .expect("backing index");
    assert!(backing.hidden);
    assert_eq!(backing.data_stream.as_deref(), Some("logs-app"));

    let details = engine.index_details("products-current", false).await.unwrap();
    assert_eq!(details.name, "products");
    assert_eq!(details.aliases, vec!["products-current"]);
    assert_eq!(details.settings["index.number_of_shards"], "1");
    assert_eq!(details.stats.as_ref().unwrap().docs_count, 300);
    let mapping = details.mapping.unwrap();
    let field = |path: &str| {
        mapping
            .fields
            .iter()
            .find(|field| field.path == path)
            .unwrap_or_else(|| panic!("{path}"))
    };
    assert_eq!(field("name.keyword").parent.as_deref(), Some("name"));
    assert_eq!(field("variants.sku").nested_path.as_deref(), Some("variants"));
    assert_eq!(field("embedding").category, FieldCategory::Vector);
    assert_eq!(field("location").category, FieldCategory::Geo);
    assert_eq!(field("created").category, FieldCategory::Date);

    let types = engine.field_types("logs-app").await.unwrap();
    assert_eq!(types["@timestamp"], FieldCategory::Date);
    assert_eq!(types["level"], FieldCategory::Keyword);
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_search_paging_and_aggregations() {
    let engine = es().await;
    // Offset paging with highlight and _source filtering.
    let page = engine
        .search(&SearchRequest {
            index: "products".into(),
            query: Some(json!({ "match": { "description": "sturdy widget" } })),
            source: Some(json!(["name", "brand"])),
            highlight: Some(json!({ "fields": { "description": {} } })),
            size: Some(10),
            track_total_hits: Some(json!(true)),
            with_field_types: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.total.as_ref().unwrap().value, 300);
    assert_eq!(page.hits.len(), 10);
    assert!(page.hits[0].highlight.as_ref().unwrap()["description"][0].contains("<em>"));
    assert!(page.hits[0].source.as_ref().unwrap().get("price").is_none());
    assert_eq!(page.field_types.as_ref().unwrap()["price"], FieldCategory::Numeric);
    assert!(engine
        .search(&SearchRequest {
            index: "products".into(),
            paging: Paging::Offset { from: 9_995 },
            size: Some(10),
            ..Default::default()
        })
        .await
        .is_err());

    // Point in time + search_after over all 20,000 log events.
    let mut request = SearchRequest {
        index: "logs-app".into(),
        sort: Some(json!([{ "@timestamp": "asc" }])),
        source: Some(json!(false)),
        size: Some(5000),
        paging: Paging::PointInTime {
            cursor: None,
            keep_alive: Some("1m".into()),
        },
        ..Default::default()
    };
    let mut seen = 0;
    let mut pages = 0;
    let mut last_pit = None;
    loop {
        let page = engine.search(&request).await.unwrap();
        pages += 1;
        seen += page.hits.len();
        last_pit = page.pit_id.clone().or(last_pit);
        match page.next {
            Some(cursor) => {
                request.paging = Paging::PointInTime {
                    cursor: Some(cursor),
                    keep_alive: Some("1m".into()),
                }
            }
            None => break,
        }
        assert!(pages < 10, "paging must end");
    }
    assert_eq!(seen, 20_000);
    engine.close_point_in_time(&last_pit.unwrap()).await.unwrap();

    // Aggregations as a tree, with typed keys.
    let page = engine
        .search(&SearchRequest {
            index: "logs-app".into(),
            size: Some(0),
            aggs: Some(json!({
                "by_service": { "terms": { "field": "service" }, "aggs": { "avg_duration": { "avg": { "field": "duration_ms" } } } },
                "errors": { "filter": { "term": { "status": 500 } } },
                "per_hour": { "date_histogram": { "field": "@timestamp", "fixed_interval": "1h" } },
                "duration": { "stats": { "field": "duration_ms" } }
            })),
            profile: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let by_service = page.aggregations.iter().find(|node| node.name == "by_service").unwrap();
    assert_eq!(by_service.agg_type, "sterms");
    let AggResult::Buckets { buckets, .. } = &by_service.result else {
        panic!("{:?}", by_service.result)
    };
    assert_eq!(buckets.len(), 4);
    assert_eq!(buckets.iter().map(|bucket| bucket.doc_count).sum::<u64>(), 20_000);
    assert!(matches!(buckets[0].children[0].result, AggResult::Value { value: Some(_), .. }));
    let errors = page.aggregations.iter().find(|node| node.name == "errors").unwrap();
    assert!(matches!(errors.result, AggResult::SingleBucket { doc_count, .. } if doc_count > 1000));
    assert!(page.profile.is_some());

    assert_eq!(
        engine
            .count("logs-app", Some(&json!({ "term": { "level": "error" } })))
            .await
            .unwrap(),
        3334
    );
    let valid = engine
        .validate_query("products", &json!({ "match": { "name": "widget" } }))
        .await
        .unwrap();
    assert!(valid.valid);
    assert!(valid.explanations[0].explanation.as_deref().unwrap_or("").contains("name:widget"));
    let invalid = engine
        .validate_query("products", &json!({ "range": { "created": { "gte": "not a date" } } }))
        .await
        .unwrap();
    assert!(!invalid.valid);
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_esql_and_sql() {
    let engine = es().await;
    let table = engine
        .esql(&EsqlRequest {
            query: "FROM logs-app | STATS events = COUNT(*), slowest = MAX(duration_ms) BY service | SORT service".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        table.columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>(),
        vec!["events", "slowest", "service"]
    );
    assert_eq!(table.columns[0].column_type, "long");
    assert_eq!(table.rows.len(), 4);
    assert!(engine
        .esql(&EsqlRequest {
            query: "FROM nope-index | LIMIT 1".into(),
            ..Default::default()
        })
        .await
        .is_err());

    let first = engine
        .sql(&SqlRequest {
            query: "SELECT service, duration_ms FROM \"logs-app\" ORDER BY duration_ms DESC".into(),
            fetch_size: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 1000);
    let cursor = first.cursor.clone().expect("a cursor for more rows");
    let second = engine.sql_next(&cursor, &first.columns).await.unwrap();
    assert_eq!(second.columns, first.columns);
    assert_eq!(second.rows.len(), 1000);
    if let Some(cursor) = second.cursor {
        engine.sql_close(&cursor).await.unwrap();
    }
    let translated = engine.sql_translate("SELECT brand FROM products WHERE price > 10").await.unwrap();
    assert!(translated.get("query").is_some());
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_documents_and_admin() {
    let engine = es().await;
    let _ = engine.apply_index_op(&IndexOp::Delete { index: TEST_INDEX.into() }).await;
    let create = IndexOp::Create {
        index: TEST_INDEX.into(),
        body: Some(
            json!({ "settings": { "number_of_replicas": 0 }, "mappings": { "properties": { "n": { "type": "integer" }, "tag": { "type": "keyword" } } } }),
        ),
    };
    assert_eq!(create.class(), RequestClass::Ddl);
    engine.apply_index_op(&create).await.unwrap();

    let refresh = WriteOptions {
        refresh: Some("true".into()),
        ..Default::default()
    };
    for n in 0..20 {
        let op = DocumentOp::Index {
            index: TEST_INDEX.into(),
            id: Some(n.to_string()),
            source: json!({ "n": n, "tag": "a" }),
            create_only: false,
            options: refresh.clone(),
        };
        assert_eq!(engine.apply_document_op(&op).await.unwrap().result, "created");
    }
    let doc = engine.get_document(TEST_INDEX, "3", None).await.unwrap();
    assert!(doc.found);
    assert_eq!(doc.source.unwrap()["n"], 3);
    // Optimistic concurrency: a stale seq_no is refused.
    let stale = WriteOptions {
        if_seq_no: Some(doc.seq_no.unwrap() + 100),
        if_primary_term: doc.primary_term,
        ..Default::default()
    };
    let update = DocumentOp::Update {
        index: TEST_INDEX.into(),
        id: "3".into(),
        doc: json!({ "tag": "b" }),
        upsert: false,
        options: stale,
    };
    assert!(engine
        .apply_document_op(&update)
        .await
        .unwrap_err()
        .to_string()
        .contains("version_conflict"));
    let update = DocumentOp::Update {
        index: TEST_INDEX.into(),
        id: "3".into(),
        doc: json!({ "tag": "b" }),
        upsert: false,
        options: refresh.clone(),
    };
    assert_eq!(engine.apply_document_op(&update).await.unwrap().result, "updated");
    let delete = DocumentOp::Delete {
        index: TEST_INDEX.into(),
        id: "4".into(),
        options: refresh.clone(),
    };
    assert_eq!(engine.apply_document_op(&delete).await.unwrap().result, "deleted");
    assert_eq!(engine.apply_document_op(&delete).await.unwrap().result, "not_found");
    assert!(!engine.get_document(TEST_INDEX, "4", None).await.unwrap().found);

    let by_update = ByQueryOp {
        kind: ByQueryKind::Update,
        index: TEST_INDEX.into(),
        query: json!({ "range": { "n": { "gte": 10 } } }),
        script: Some(json!({ "source": "ctx._source.tag = 'c'" })),
        proceed_on_conflicts: true,
        background: false,
        refresh: true,
        max_docs: None,
    };
    assert_eq!(engine.by_query(&by_update).await.unwrap().updated, 10);
    let by_delete = ByQueryOp {
        kind: ByQueryKind::Delete,
        query: json!({ "term": { "tag": "c" } }),
        script: None,
        ..by_update
    };
    assert_eq!(by_delete.class(), RequestClass::Destructive);
    assert_eq!(engine.by_query(&by_delete).await.unwrap().deleted, 10);
    assert_eq!(engine.count(TEST_INDEX, None).await.unwrap(), 9);

    // Analyze, ingest simulate, _cat, cluster stats, tasks.
    let analyzed = engine
        .analyze(&AnalyzeRequest {
            analyzer: Some("english".into()),
            text: vec!["The quick foxes jumped".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        analyzed.tokens.iter().map(|token| token.token.as_str()).collect::<Vec<_>>(),
        vec!["quick", "fox", "jump"]
    );
    let simulated = engine
        .simulate_pipeline(
            &PipelineRef::Id("logs-pipeline".into()),
            &[json!({ "level": "ERROR", "msg": "boom" })],
            false,
        )
        .await
        .unwrap();
    let source = simulated[0].source.as_ref().unwrap();
    assert_eq!(
        (source["level"].as_str(), source["message"].as_str()),
        (Some("error"), Some("boom"))
    );
    let inline = engine
        .simulate_pipeline(
            &PipelineRef::Inline(json!({ "processors": [{ "set": { "field": "x", "value": 1 } }, { "fail": { "message": "stop" } }] })),
            &[json!({})],
            true,
        )
        .await
        .unwrap();
    assert_eq!(inline[0].error.as_deref(), Some("stop"));
    assert_eq!(inline[0].processor_results.as_ref().unwrap().len(), 2);

    for api in [CatApi::Nodes, CatApi::Shards, CatApi::Allocation, CatApi::ThreadPool] {
        let table = engine.cat(api).await.unwrap();
        assert!(!table.rows.is_empty(), "{api:?}");
    }
    let stats = engine.cluster_stats().await.unwrap();
    assert_eq!(stats.nodes, 1);
    assert!(stats.docs >= 20_000);
    let tasks = engine.list_tasks(None).await.unwrap();
    assert!(tasks.iter().any(|task| task.action.starts_with("cluster:monitor/tasks/lists")));
    let caps = engine.field_caps(TEST_INDEX, None).await.unwrap();
    assert!(caps.iter().any(|field| field.name == "tag" && field.types[0].aggregatable));

    let close = IndexOp::Close { index: TEST_INDEX.into() };
    assert_eq!(close.class(), RequestClass::Destructive);
    engine.apply_index_op(&close).await.unwrap();
    engine.apply_index_op(&IndexOp::Open { index: TEST_INDEX.into() }).await.unwrap();
    engine
        .apply_index_op(&IndexOp::ForceMerge {
            index: TEST_INDEX.into(),
            max_num_segments: Some(1),
            only_expunge_deletes: false,
        })
        .await
        .unwrap();
    engine.apply_index_op(&IndexOp::Delete { index: TEST_INDEX.into() }).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_engine_elasticsearch_dev_tools_console() {
    let engine = es().await;
    let input = r#"
# Dev Tools console
PUT fosta-test-console
{ "settings": { "number_of_replicas": 0 } }

POST _bulk?refresh=true
{ "index": { "_index": "fosta-test-console", "_id": "1" } }
{ "title": """multi
line""" }
{ "index": { "_index": "fosta-test-console", "_id": "2" } }
{ "title": "two" }

GET fosta-test-console/_search
{ "query": { "match_all": {} } }

GET fosta-test-console/_doc/404

DELETE fosta-test-console
"#;
    let results = engine.run_console(input, Duration::from_secs(30)).await.unwrap();
    assert_eq!(results.len(), 5);
    let classes: Vec<_> = results.iter().map(|result| result.request.classification.class).collect();
    assert_eq!(
        classes,
        vec![
            RequestClass::Ddl,
            RequestClass::Write,
            RequestClass::Read,
            RequestClass::Read,
            RequestClass::Destructive
        ]
    );
    let status: Vec<_> = results.iter().map(|result| result.response.as_ref().unwrap().status).collect();
    assert_eq!(status, vec![200, 200, 200, 404, 200]);
    assert_eq!(results[1].response.as_ref().unwrap().body["errors"], false);
    assert_eq!(results[2].response.as_ref().unwrap().body["hits"]["total"]["value"], 2);
    assert_eq!(results[1].request.line, 5);

    // Timeouts are per request.
    let slow = engine
        .run_console("GET _cluster/health?wait_for_status=red&timeout=5s", Duration::from_millis(300))
        .await
        .unwrap();
    assert!(
        slow[0].error.as_deref().unwrap_or("").contains("did not answer"),
        "{:?}",
        slow[0].error
    );
}

#[tokio::test]
#[ignore]
async fn live_engine_opensearch_smoke() {
    let Ok(engine) = EsEngine::connect(&EsConnectOptions::single(OS_URL)).await else {
        eprintln!("OpenSearch is not running on {OS_URL}; skipped");
        return;
    };
    assert_eq!(engine.info().distribution, Distribution::OpenSearch);
    let catalog = engine.catalog(&CatalogOptions::default()).await;
    assert!(catalog.indices.items.iter().any(|index| index.name == "products"));
    assert!(catalog.data_streams.items.iter().any(|stream| stream.name == "logs-app"));
    let policies = &catalog.lifecycle_policies;
    assert!(
        policies
            .items
            .iter()
            .any(|policy| policy.name == "logs-policy" && policy.stages == vec!["hot", "delete"]),
        "{policies:?}"
    );

    let mut request = SearchRequest {
        index: "logs-app".into(),
        sort: Some(json!([{ "@timestamp": "asc" }])),
        size: Some(500),
        paging: Paging::PointInTime {
            cursor: None,
            keep_alive: None,
        },
        ..Default::default()
    };
    let mut seen = 0;
    let mut pit = None;
    loop {
        let page = engine.search(&request).await.unwrap();
        seen += page.hits.len();
        pit = page.pit_id.clone().or(pit);
        match page.next {
            Some(cursor) => {
                request.paging = Paging::PointInTime {
                    cursor: Some(cursor),
                    keep_alive: None,
                }
            }
            None => break,
        }
    }
    assert_eq!(seen, 2000);
    engine.close_point_in_time(&pit.unwrap()).await.unwrap();

    let table = engine
        .sql(&SqlRequest {
            query: "SELECT brand, price FROM products".into(),
            fetch_size: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(table.rows.len(), 100);
    let next = engine
        .sql_next(table.cursor.as_deref().expect("cursor"), &table.columns)
        .await
        .unwrap();
    assert_eq!(next.rows.len(), 100);
    assert!(engine
        .esql(&EsqlRequest {
            query: "FROM products".into(),
            ..Default::default()
        })
        .await
        .is_err());
}
