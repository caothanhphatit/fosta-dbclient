//! Contract suites for ClickHouse against `dev/compose/clickhouse.yml` (data from `dev/seed/clickhouse/contract.sql`),
//! plus ClickHouse-specific checks: edits only on MergeTree tables, the asynchronous-mutation guard, the lazy tree with
//! sizes, the operations behind the ClickHouse views, error positions, read-only sessions and the database of a run.
//! Override the server with FOSTA_CONTRACT_CLICKHOUSE_HOST / _PORT / _USER / _PASSWORD.
//! Run: `cargo test contract_clickhouse -- --ignored --test-threads=1`.

use std::sync::Arc;

use crate::{
    contract::{
        testing::{suites::*, vector::*},
        BrowseRequest, CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Field, Filter, NodeKind, ObjectRef,
        RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session, SessionOptions, TreeRequest, Value,
    },
    error::AppResult,
    runtime::{ConnectionManager, Handle},
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_CLICKHOUSE_{name}")).unwrap_or_else(|_| default.to_owned())
}

/// The chunks of the seed: lines that read `GO` separate them (whatever the line endings).
fn seed() -> Vec<String> {
    let mut chunks = vec![String::new()];
    for line in include_str!("../../../../dev/seed/clickhouse/contract.sql").lines() {
        if line.trim() == "GO" {
            chunks.push(String::new());
        } else if let Some(chunk) = chunks.last_mut() {
            chunk.push_str(line);
            chunk.push('\n');
        }
    }
    chunks
        .into_iter()
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("`id` = {key}"),
    }
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "clickhouse".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "8123").parse().unwrap_or(8123),
        user: env("USER", "studio"),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-root"))
}

fn items() -> ObjectRef {
    ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::TABLE)
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "clickhouse",
        spec: spec(),
        secrets: secrets(),
        table: items(),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        // sleepEachRow sleeps at most 3 s per block, so one row per block gives a statement that lasts 30 s.
        sleep_statement: Some("SELECT sleepEachRow(0.5) FROM numbers(60) SETTINGS max_block_size = 1"),
        syntax_error: "SELEC * FORM contract_items",
        // The table has a CHECK constraint on the price (ClickHouse has no unique keys).
        constraint_violation: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (1, 'dup', -1, 1)",
        multi_statement: "SELECT 1; SELECT id, name FROM fosta_contract.contract_items ORDER BY id LIMIT 3",
        insert_probe: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT count() FROM fosta_contract.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("SHOW TABLES", Category::Read),
            ("DESCRIBE TABLE t", Category::Read),
            ("EXPLAIN SELECT 1", Category::Read),
            ("INSERT INTO t VALUES (1)", Category::Write),
            ("ALTER TABLE t UPDATE a = 1 WHERE id = 2", Category::Write),
            ("ALTER TABLE t DELETE WHERE id = 2", Category::Write),
            ("DELETE FROM t WHERE id = 2", Category::Write),
            ("CREATE TABLE x (id UInt64) ENGINE = MergeTree ORDER BY id", Category::Ddl),
            ("DROP TABLE t", Category::Destructive),
            ("TRUNCATE TABLE t", Category::Destructive),
            ("ALTER TABLE t DROP PARTITION 202501", Category::Destructive),
            ("SYSTEM FLUSH LOGS", Category::Admin),
        ],
        language: "sql",
        // ClickHouse has no foreign keys.
        gaps: &[Gap::ForeignKeys],
    }
}

crate::contract_tests!(contract_clickhouse, fixture());

// --- ClickHouse-specific checks -----------------------------------------------------------------------------------------

async fn connected() -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect("ch", spec(), secrets()).await.expect("connect");
    let handle = manager.handle("ch").unwrap();
    // Seed through a session, like the suites do.
    let session = open(&handle, None).await;
    for chunk in seed() {
        run(&handle, &session, &chunk, None, RunOptions::default()).await.expect("seed");
    }
    (manager, handle)
}

async fn open(handle: &Handle, database: Option<&str>) -> Arc<dyn Session> {
    let options = SessionOptions {
        language: None,
        database: database.map(str::to_owned),
    };
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&options, &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(
    handle: &Handle,
    session: &Arc<dyn Session>,
    text: &str,
    database: Option<&str>,
    options: RunOptions,
) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, options, &mut sink).await?;
    Ok(sink)
}

async fn apply(handle: &Handle, set: RowChangeSet) -> AppResult<crate::contract::ApplyResult> {
    handle
        .call("applied", |conn| async move {
            conn.tabular().unwrap().apply(&set, &CallContext::default()).await
        })
        .await
}

async fn browse(handle: &Handle, request: BrowseRequest) -> AppResult<crate::contract::Page> {
    handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&request, &CallContext::default()).await
        })
        .await
}

async fn invoke(handle: &Handle, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
    let name = name.to_owned();
    handle
        .call(
            "invoked",
            |conn| async move { conn.invoke(&name, args, &CallContext::default()).await },
        )
        .await
}

fn text(value: &Value) -> String {
    value.display().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_tree_shows_engine_rows_and_size() {
    let (_manager, handle) = connected().await;
    let children = |parent| {
        let handle = handle.clone();
        async move {
            let request = TreeRequest {
                parent,
                ..TreeRequest::default()
            };
            handle
                .call(
                    "listed",
                    |conn| async move { conn.children(&request, &CallContext::default()).await },
                )
                .await
                .expect("children")
        }
    };
    let top = children(None).await;
    let names: Vec<&str> = top.nodes.iter().map(|node| node.label.as_str()).collect();
    assert_eq!(names.last(), Some(&"system"), "system database last: {names:?}");
    let database = top.nodes.iter().find(|node| node.label == "fosta_contract").expect("database");
    let folders = children(Some(database.key.clone())).await;
    assert_eq!(
        folders.nodes.iter().map(|node| node.label.as_str()).collect::<Vec<_>>(),
        vec!["Tables"],
        "empty folders are left out"
    );
    let tables = children(Some(folders.nodes[0].key.clone())).await;
    let item = tables.nodes.iter().find(|node| node.label == "contract_items").expect("table");
    let detail = item.detail.clone().unwrap_or_default();
    assert!(
        detail.starts_with("MergeTree · 1K rows") || detail.starts_with("MergeTree · 1000 rows"),
        "{detail}"
    );
    assert!(detail.ends_with("KiB") || detail.ends_with(" B"), "{detail}");
    assert!(item.badges.is_empty(), "MergeTree tables are editable");
    assert_eq!(item.key.id, "db:fosta_contract/folder:tables/table:contract_items");
    // The shop database of the dev seed has all four kinds; Log tables and views carry no edit rights.
    if let Some(shop) = top.nodes.iter().find(|node| node.label == "shop") {
        let folders = children(Some(shop.key.clone())).await;
        let labels: Vec<&str> = folders.nodes.iter().map(|node| node.label.as_str()).collect();
        assert!(
            labels.contains(&"Views") && labels.contains(&"Materialized views") && labels.contains(&"Dictionaries"),
            "{labels:?}"
        );
    }
    // The filter applies to labels.
    let request = TreeRequest {
        parent: Some(folders.nodes[0].key.clone()),
        filter: Some("CHILD".into()),
        ..TreeRequest::default()
    };
    let filtered = handle
        .call(
            "filtered",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .unwrap();
    assert_eq!(filtered.nodes.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_only_mergetree_tables_are_editable() {
    let (_manager, handle) = connected().await;
    let session = open(&handle, None).await;
    let log = ObjectRef::new(Some("fosta_contract"), None, "contract_log", NodeKind::TABLE);
    for sql in [
        "DROP TABLE IF EXISTS fosta_contract.contract_log SYNC",
        "CREATE TABLE fosta_contract.contract_log (id Int32, name String) ENGINE = Log",
        "INSERT INTO fosta_contract.contract_log VALUES (1, 'a')",
    ] {
        run(&handle, &session, sql, None, RunOptions::default()).await.expect(sql);
    }
    // Describe marks every column read-only and says why.
    let description = handle
        .call("described", |conn| {
            let object = log.clone();
            async move { conn.describe(&object, &CallContext::default()).await }
        })
        .await
        .unwrap();
    assert!(description.columns.iter().all(|column| column.hints.read_only));
    assert!(
        description
            .extra
            .iter()
            .any(|(name, text)| name == "edits" && text.starts_with("read-only")),
        "{:?}",
        description.extra
    );
    let insert = RowChange::Insert {
        values: vec![Field::new("id", Value::Int(2)), Field::new("name", Value::Text("b".into()))],
    };
    let error = apply(
        &handle,
        RowChangeSet {
            object: log.clone(),
            changes: vec![insert],
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("Log engine") && error.to_string().contains("read-only"),
        "{error}"
    );
    // Reading still works, and nothing was written.
    let page = browse(&handle, BrowseRequest::new(log, 10)).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    // The MergeTree table says that its edits are mutations.
    let table = items();
    let description = handle
        .call(
            "described",
            |conn| async move { conn.describe(&table, &CallContext::default()).await },
        )
        .await
        .unwrap();
    assert!(description
        .extra
        .iter()
        .any(|(name, text)| name == "edits" && text.starts_with("asynchronous mutations")));
    run(
        &handle,
        &session,
        "DROP TABLE IF EXISTS fosta_contract.contract_log SYNC",
        None,
        RunOptions::default(),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_edits_check_nullable_originals_and_duplicate_keys() {
    let (_manager, handle) = connected().await;
    let key = |id: i64| vec![Field::new("id", Value::Int(id))];
    // Row 3 has a NULL note. An update guarded by the NULL original applies; one guarded by a wrong note conflicts.
    let update = |original: Value, note: &str| RowChange::Update {
        key: key(3),
        values: vec![Field::new("note", Value::Text(note.to_owned()))],
        original: vec![Field::new("note", original)],
        version: None,
        force: false,
    };
    let error = apply(
        &handle,
        RowChangeSet {
            object: items(),
            changes: vec![update(Value::Text("not null".into()), "x")],
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Some("CONFLICT"), "{error}");
    let done = apply(
        &handle,
        RowChangeSet {
            object: items(),
            changes: vec![update(Value::Null, "now set")],
        },
    )
    .await
    .unwrap();
    assert_eq!(done.updated, 1);
    let mut request = BrowseRequest::new(items(), 5);
    request.filter = key_filter(3);
    assert_eq!(text(&browse(&handle, request.clone()).await.unwrap().rows[0][7]), "now set");
    // A key that matches two rows is refused: the primary key is not unique.
    let session = open(&handle, None).await;
    run(
        &handle,
        &session,
        "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (7, 'twin', 1, 0)",
        None,
        RunOptions::default(),
    )
    .await
    .unwrap();
    let error = apply(
        &handle,
        RowChangeSet {
            object: items(),
            changes: vec![RowChange::Delete {
                key: key(7),
                original: vec![],
                version: None,
                force: false,
            }],
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("matched 2 rows"), "{error}");
    request.filter = key_filter(7);
    assert_eq!(browse(&handle, request).await.unwrap().rows.len(), 2, "nothing was deleted");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_browse_totals() {
    let (_manager, handle) = connected().await;
    let mut request = BrowseRequest::new(items(), 5);
    request.exact_total = true;
    assert_eq!(
        browse(&handle, request.clone())
            .await
            .unwrap()
            .total
            .map(|total| (total.value, total.exact)),
        Some((1000, true))
    );
    request.filter = Filter::Raw {
        language: RawFilterLang::Sql,
        text: "id <= 10".into(),
    };
    assert_eq!(
        browse(&handle, request.clone())
            .await
            .unwrap()
            .total
            .map(|total| (total.value, total.exact)),
        Some((10, true))
    );
    request.exact_total = false;
    request.estimate_total = true;
    assert_eq!(
        browse(&handle, request).await.unwrap().total.map(|total| total.value),
        Some(10),
        "small tables are counted"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_operations_for_the_views() {
    let (_manager, handle) = connected().await;
    let args = serde_json::json!({ "database": "fosta_contract", "table": "contract_items" });
    let parts = invoke(&handle, "clickhouse.parts", args.clone()).await.unwrap();
    assert_eq!(parts["columns"][0], "part");
    assert!(parts["rows"].as_array().is_some_and(|rows| !rows.is_empty()));
    // A mutation shows up with its command, and finishes (the edits wait for it).
    apply(
        &handle,
        RowChangeSet {
            object: items(),
            changes: vec![RowChange::Update {
                key: vec![Field::new("id", Value::Int(4))],
                values: vec![Field::new("note", Value::Text("mutated".into()))],
                original: vec![],
                version: None,
                force: false,
            }],
        },
    )
    .await
    .unwrap();
    let mutations = invoke(&handle, "clickhouse.mutations", args).await.unwrap();
    let columns: Vec<&str> = mutations["columns"].as_array().unwrap().iter().filter_map(|c| c.as_str()).collect();
    let command = columns.iter().position(|c| *c == "command").unwrap();
    let done = columns.iter().position(|c| *c == "is_done").unwrap();
    let first = &mutations["rows"][0];
    assert!(first[command].as_str().unwrap().contains("UPDATE"), "{first}");
    assert_eq!(first[done], "1");
    // Statements run before are in the query log once it is flushed; the log can also say it has nothing yet.
    let session = open(&handle, None).await;
    run(&handle, &session, "SYSTEM FLUSH LOGS", None, RunOptions::default())
        .await
        .unwrap();
    let log = invoke(&handle, "clickhouse.queryLog", serde_json::json!({ "limit": 20 }))
        .await
        .unwrap();
    assert!(log["rows"].as_array().is_some_and(|rows| !rows.is_empty()), "{log}");
    assert!(log["columns"].as_array().unwrap().iter().any(|c| c == "query_duration_ms"));
    // Running queries and KILL: a long statement is listed, then stopped by its query id.
    let long = {
        let handle = handle.clone();
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            run(
                &handle,
                &session,
                "SELECT sleepEachRow(0.5) FROM numbers(60) SETTINGS max_block_size = 1",
                None,
                RunOptions::default(),
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let processes = invoke(&handle, "clickhouse.processes", serde_json::json!({})).await.unwrap();
    let columns: Vec<&str> = processes["columns"].as_array().unwrap().iter().filter_map(|c| c.as_str()).collect();
    let (id, query) = (
        columns.iter().position(|c| *c == "query_id").unwrap(),
        columns.iter().position(|c| *c == "query").unwrap(),
    );
    let row = processes["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row[query].as_str().is_some_and(|q| q.contains("sleepEachRow")))
        .unwrap_or_else(|| panic!("the long query is not listed: {processes}"));
    invoke(&handle, "clickhouse.kill", serde_json::json!({ "queryId": row[id] }))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), long)
        .await
        .expect("stopped")
        .unwrap();
    assert_eq!(outcome.unwrap_err().code(), Some("CANCELLED"));
    assert!(
        invoke(&handle, "clickhouse.kill", serde_json::json!({})).await.is_err(),
        "kill needs an id"
    );
    assert!(invoke(&handle, "clickhouse.nope", serde_json::json!({})).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_errors_sessions_and_limits() {
    let (_manager, handle) = connected().await;
    let session = open(&handle, Some("fosta_contract")).await;
    // The tab's database applies to unqualified names; a run's own database wins.
    let sink = run(&handle, &session, "SELECT count() FROM contract_items", None, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(text(&sink.sets[0].rows[0][0]), "1000");
    let sink = run(&handle, &session, "SELECT currentDatabase()", Some("system"), RunOptions::default())
        .await
        .unwrap();
    assert_eq!(text(&sink.sets[0].rows[0][0]), "system");
    // A syntax error carries the code, the failing statement and a position inside the script.
    let error = run(&handle, &session, "SELECT 1; SELEC 2", None, RunOptions::default())
        .await
        .unwrap_err();
    let detail = error.payload().unwrap();
    assert_eq!(detail["vendorCode"], "62", "{detail}");
    assert_eq!(detail["statement"]["index"], 1);
    assert!(detail["position"]["offset"].as_u64().is_some_and(|offset| offset >= 10), "{detail}");
    assert!(
        detail["detail"].as_str().is_some_and(|text| text.starts_with("Expected")),
        "{detail}"
    );
    // A missing table has its own code.
    let error = run(&handle, &session, "SELECT * FROM nope_table", None, RunOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.payload().unwrap()["vendorCode"], "60");
    // The read-only rule refuses writes but still lets the statement timeout apply.
    let options = RunOptions {
        read_only: true,
        timeout: Some(std::time::Duration::from_secs(5)),
        ..RunOptions::default()
    };
    run(&handle, &session, "SELECT 1", None, options.clone())
        .await
        .expect("reads work in a read-only session");
    let error = run(
        &handle,
        &session,
        "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (9000, 'ro', 1, 1)",
        None,
        options.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.payload().unwrap()["vendorCode"], "164", "{error}");
    // The statement timeout is the server's.
    let slow = RunOptions {
        timeout: Some(std::time::Duration::from_secs(1)),
        ..RunOptions::default()
    };
    let error = run(
        &handle,
        &session,
        "SELECT sleepEachRow(0.5) FROM numbers(60) SETTINGS max_block_size = 1",
        None,
        slow,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Some("TIMEOUT"), "{error}");
    // And the tab keeps working.
    run(&handle, &session, "SELECT 1", None, RunOptions::default()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_clickhouse_creates_databases() {
    let (_manager, handle) = connected().await;
    let session = open(&handle, None).await;
    let _ = run(
        &handle,
        &session,
        "DROP DATABASE IF EXISTS fosta_contract_new SYNC",
        None,
        RunOptions::default(),
    )
    .await;
    handle
        .call("created", |conn| async move {
            conn.admin()
                .unwrap()
                .create_database("fosta_contract_new", None, &CallContext::default())
                .await
        })
        .await
        .unwrap();
    let sink = run(&handle, &session, "EXISTS DATABASE fosta_contract_new", None, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(text(&sink.sets[0].rows[0][0]), "1");
    run(
        &handle,
        &session,
        "DROP DATABASE fosta_contract_new SYNC",
        None,
        RunOptions::default(),
    )
    .await
    .unwrap();
}

/// The shared vector suite on `fosta_contract.contract_vectors` (`Array(Float32)`, 4 dimensions, an HNSW cosine
/// `vector_similarity` index), plus ClickHouse specifics.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/clickhouse.yml"]
async fn contract_clickhouse_vector() {
    let vectors = VectorFixture {
        database: "fosta_contract",
        table: "contract_vectors",
        column: "embedding",
        dims: 4,
        key_column: "id",
        key: 1,
        indexed: true,
    };
    let suite = suite_vector(&fixture(), &vectors).await.expect("ClickHouse has vector functions");
    assert_eq!(suite.info["columns"][0]["typeName"], "Array(Float32)");
    let index = &suite.info["indexes"][0];
    assert_eq!(index["name"], "embedding_idx");
    assert_eq!((index["kind"].as_str(), index["metric"].as_str()), (Some("hnsw"), Some("cosine")));
    assert_eq!(suite.info["metrics"], serde_json::json!(["cosine", "euclidean", "dot"]));
    let search = &suite.search;
    let sql = search["sql"].as_str().unwrap();
    assert!(sql.contains("cosineDistance(`embedding`, {query:Array(Float32)})"), "{search}");
    assert_eq!(search["indexUsed"], "embedding_idx", "{search}");
    let columns = search["result"]["columns"].as_array().unwrap();
    assert!(
        columns.iter().all(|column| column["name"] != "embedding"),
        "vector columns are not selected"
    );

    let (_manager, handle) = connected().await;
    let session = open(&handle, None).await;
    let search = |table: &str, text: &str, metric: &str| {
        serde_json::json!({ "database": "fosta_contract", "table": table, "column": "embedding", "k": 3, "metric": metric,
                            "filter": null, "query": { "kind": "literal", "text": text } })
    };
    // A query vector of integers is cast to Array(Float32), so the index still serves it.
    let integers = invoke(&handle, "vector.search", search("contract_vectors", "[1, 0, 0, 1]", "auto"))
        .await
        .unwrap();
    assert_eq!(integers["indexUsed"], "embedding_idx", "{integers}");
    assert!(
        integers["runnableSql"]
            .as_str()
            .unwrap()
            .contains("CAST([1,0,0,1] AS Array(Float32))"),
        "{integers}"
    );

    // The recall's top-k through the index orders by the bare distance: its plan names the index.
    let column = crate::contract::vector::VectorColumn {
        name: "embedding".into(),
        dims: 4,
        type_name: "Array(Float32)".into(),
        metrics: Vec::new(),
    };
    let index = crate::contract::vector::VectorIndex {
        name: "embedding_idx".into(),
        column: "embedding".into(),
        metric: "cosine".into(),
        kind: "hnsw".into(),
        m: None,
        params: "dims=4".into(),
    };
    let key = ["id".to_owned()];
    let table = crate::contract::vector::TableRef::parse("fosta_contract", "contract_vectors", false);
    let plan_of = |exact| {
        let request = crate::contract::vector::NearestRequest {
            column: &column,
            key: &key,
            index: &index,
            query: &[1.0, 0.0, 0.0, 1.0],
            k: 5,
            exact,
        };
        format!("EXPLAIN indexes = 1 {}", super::vector::nearest_sql(&table, &request))
    };
    for (exact, expected) in [(false, Some("embedding_idx")), (true, None)] {
        let sink = run(&handle, &session, &plan_of(exact), None, RunOptions::default()).await.unwrap();
        let lines: Vec<String> = sink.sets[0].rows.iter().map(|row| text(&row[0])).collect();
        assert_eq!(
            super::vector::index_in_plan(&lines).as_deref(),
            expected,
            "exact={exact}: {lines:#?}"
        );
    }

    // Rows of another length (an empty array) are skipped where no index serves the search, and where an index was
    // added after them (the search runs again without the index).
    for statement in [
        "DROP TABLE IF EXISTS fosta_contract.contract_vectors_ragged SYNC",
        "CREATE TABLE fosta_contract.contract_vectors_ragged (id Int32, embedding Array(Float32)) ENGINE = MergeTree ORDER BY id",
        "INSERT INTO fosta_contract.contract_vectors_ragged VALUES (1, [1, 0, 0, 0]), (2, []), (3, [1, 2]), (4, [0, 1, 0, 0])",
        "DROP TABLE IF EXISTS fosta_contract.contract_vectors_late SYNC",
        "CREATE TABLE fosta_contract.contract_vectors_late (id Int32, embedding Array(Float32)) ENGINE = MergeTree ORDER BY id",
        "INSERT INTO fosta_contract.contract_vectors_late VALUES (1, [1, 0, 0, 0]), (2, []), (3, [1, 2])",
        "ALTER TABLE fosta_contract.contract_vectors_late ADD INDEX embedding_idx embedding TYPE vector_similarity('hnsw', 'cosineDistance', 4)",
        "INSERT INTO fosta_contract.contract_vectors_late VALUES (4, [0, 1, 0, 0])",
    ] {
        run(&handle, &session, statement, None, RunOptions::default())
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    for (table, metric) in [
        ("contract_vectors_ragged", "cosine"),
        ("contract_vectors_ragged", "euclidean"),
        ("contract_vectors_late", "auto"),
    ] {
        let found = invoke(&handle, "vector.search", search(table, "[1, 1, 0, 0]", metric))
            .await
            .unwrap_or_else(|error| panic!("{table} {metric}: {error}"));
        let ids: Vec<String> = found["result"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| text_json(&row[1]))
            .collect();
        assert_eq!(ids.len(), 2, "{table} {metric}: {found}");
        assert!(ids.iter().all(|id| id == "1" || id == "4"), "{table} {metric}: {found}");
        assert!(
            found["runnableSql"].as_str().unwrap().contains("length(`embedding`) = 4"),
            "{table} {metric}: {found}"
        );
    }
    let late = invoke(&handle, "vector.search", search("contract_vectors_late", "[1, 1, 0, 0]", "auto"))
        .await
        .unwrap();
    assert!(
        late["plan"]["rows"][0][0]["v"]
            .as_str()
            .unwrap()
            .starts_with("Some rows of embedding hold an array of another length"),
        "{late}"
    );
    for table in ["contract_vectors_ragged", "contract_vectors_late"] {
        run(
            &handle,
            &session,
            &format!("DROP TABLE fosta_contract.{table} SYNC"),
            None,
            RunOptions::default(),
        )
        .await
        .unwrap();
    }
}

/// A typed cell of a JSON result set as text (`{"t":"int","v":1}` → `1`).
fn text_json(value: &serde_json::Value) -> String {
    match &value["v"] {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
