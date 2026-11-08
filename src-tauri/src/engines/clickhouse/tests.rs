//! Live tests against the `clickhouse` service of dev/docker-compose.engines-sql.yml. Run with
//! `cargo test live_engine_ -- --ignored --test-threads=1`.

use super::*;
use crate::engines::common::{
    parse_input, CollectSink, ErrorKind, FilterCond, FilterOp, RunOptions, SortKey, StatementKind, TxState, ValueKind,
};

fn opts() -> ChOptions {
    ChOptions::parse("clickhouse://studio:studio-root@127.0.0.1:8123/shop").unwrap()
}

async fn engine() -> ChEngine {
    ChEngine::connect(opts())
        .await
        .expect("connect to the dev ClickHouse (docker compose -f dev/docker-compose.engines-sql.yml up -d)")
}

fn t(name: &str) -> ObjectRef {
    ObjectRef::new(Some("shop"), None, name, NodeKind::Table)
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_connect_and_version() {
    let e = engine().await;
    assert!(e.server_version().starts_with("25."), "{}", e.server_version());
    let mut bad = opts();
    bad.password = Some("wrong".into());
    let err = ChEngine::connect(bad).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::Connect, "{err}");
    let mut closed = opts();
    closed.port = 1;
    assert_eq!(ChEngine::connect(closed).await.err().unwrap().kind, ErrorKind::Connect);
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_catalog_tree() {
    let e = engine().await;
    let tree = e.catalog().await.unwrap();
    let shop = tree.iter().find(|n| n.name == "shop").expect("shop");
    let orders = shop.find(NodeKind::Table, "orders").unwrap();
    assert_eq!(orders.detail.as_deref(), Some("MergeTree"));
    assert_eq!(
        shop.find(NodeKind::Table, "products").unwrap().detail.as_deref(),
        Some("ReplacingMergeTree")
    );
    assert!(shop.find(NodeKind::View, "order_totals").is_some());
    assert!(shop.find(NodeKind::MaterializedView, "daily_revenue_mv").is_some());
    assert!(shop.find(NodeKind::Dictionary, "category_dict").is_some());
    assert_eq!(tree.last().map(|n| n.name.as_str()), Some("system"), "system database last");
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_describe() {
    let e = engine().await;
    let orders = e.describe(&t("orders")).await.unwrap();
    assert_eq!(orders.primary_key.as_ref().unwrap().columns, vec!["customer_id", "id"]);
    assert_eq!(orders.row_estimate, Some(4000));
    assert!(orders.extra.contains(&("engine".into(), "MergeTree".into())));
    assert!(orders.extra.contains(&("partitionKey".into(), "toYYYYMM(ordered_at)".into())));
    assert!(orders.column("note").unwrap().nullable);
    assert_eq!(orders.column("status").unwrap().kind, ValueKind::Text);
    assert!(orders.ddl.unwrap().starts_with("CREATE TABLE shop.orders"));
    let customers = e.describe(&t("customers")).await.unwrap();
    assert_eq!(customers.column("id").unwrap().comment.as_deref(), Some("Customer number"));
    assert_eq!(customers.column("country").unwrap().default.as_deref(), Some("'VN'"));
    let cats = e.describe(&t("categories")).await.unwrap();
    assert_eq!(cats.comment.as_deref(), Some("Product categories"));
    let dict = e
        .describe(&ObjectRef::new(Some("shop"), None, "category_dict", NodeKind::Dictionary))
        .await
        .unwrap();
    assert_eq!(dict.object.kind, NodeKind::Dictionary);
    assert!(dict.ddl.unwrap().starts_with("CREATE DICTIONARY shop.category_dict"));
    let mv = e
        .describe(&ObjectRef::new(Some("shop"), None, "daily_revenue_mv", NodeKind::View))
        .await
        .unwrap();
    assert_eq!(mv.object.kind, NodeKind::MaterializedView);
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_browse_filters_and_keyset() {
    let e = engine().await;
    let mut req = BrowseRequest::new(t("orders"), 100);
    req.filter = vec![
        FilterCond::new("status", FilterOp::In, &["paid", "shipped"]),
        FilterCond::new("ordered_at", FilterOp::Ge, &["2025-03-05 00:00:00"]),
        FilterCond::new("note", FilterOp::IsNull, &[]),
    ];
    req.estimate_total = true;
    let first = e.browse(&req).await.unwrap();
    assert_eq!(first.rows.len(), 100);
    assert!(first.sql.contains("{p1:Enum8("), "{}", first.sql);
    assert!(first.estimated_total.is_some());
    assert_eq!(first.key_columns, vec!["customer_id", "id"]);
    let mut next = req.clone();
    next.after = first.next_keyset.clone();
    let mut by_offset = req.clone();
    by_offset.offset = 100;
    assert_eq!(e.browse(&next).await.unwrap().rows, e.browse(&by_offset).await.unwrap().rows);

    let mut req = BrowseRequest::new(t("customers"), 10);
    req.filter = vec![
        FilterCond::new("email", FilterOp::Contains, &["CUSTOMER19"]),
        FilterCond::new("vip", FilterOp::Eq, &["false"]),
    ];
    req.sort = vec![SortKey::desc("created_at")];
    let p = e.browse(&req).await.unwrap();
    assert!(!p.rows.is_empty());
    req.filter = vec![FilterCond::new("full_name", FilterOp::Eq, &["x' OR 1=1 --"])];
    assert!(e.browse(&req).await.unwrap().rows.is_empty());
    let mut req = BrowseRequest::new(t("products"), 10);
    req.filter = vec![FilterCond::new("tags", FilterOp::Eq, &[r#"["tag1", "tag1"]"#])];
    assert!(!e.browse(&req).await.unwrap().rows.is_empty());
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_typed_values_round_trip() {
    let e = engine().await;
    let mut req = BrowseRequest::new(t("type_zoo"), 10);
    req.sort = vec![SortKey::asc("id")];
    let page = e.browse(&req).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    let get = |name: &str| page.rows[0][page.columns.iter().position(|c| c.name == name).unwrap()].clone();
    let shown = |name: &str| get(name).display().unwrap();
    assert_eq!(get("c_bool"), Value::Bool(true));
    assert_eq!(get("c_int8"), Value::Int(-128));
    assert_eq!(get("c_int64"), Value::Int(i64::MAX));
    assert_eq!(get("c_int128"), Value::Decimal("-170141183460469231731687303715884105728".into()));
    assert_eq!(get("c_uint64"), Value::Decimal("18446744073709551615".into()));
    assert_eq!(
        get("c_uint256"),
        Value::Decimal("115792089237316195423570985008687907853269984665640564039457584007913129639935".into())
    );
    assert_eq!(get("c_float32"), Value::Float(1.5));
    assert_eq!(get("c_float64"), Value::Float(std::f64::consts::E));
    assert_eq!(get("c_decimal"), Value::Decimal("12345678901234.123456".into()));
    assert_eq!(get("c_string"), Value::Text("text with 'quotes' and unicode: Xin chào".into()));
    assert_eq!(get("c_fixed"), Value::Text("ab".into()));
    assert_eq!(get("c_lowcard"), Value::Text("low".into()));
    assert_eq!(shown("c_date"), "2026-02-28");
    assert_eq!(shown("c_date32"), "1925-06-01");
    assert_eq!(shown("c_datetime"), "2026-02-28 13:45:30");
    assert_eq!(shown("c_datetime64"), "2026-02-28 13:45:30.123456");
    assert_eq!(get("c_uuid"), Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()));
    assert_eq!(shown("c_ipv4"), "192.168.1.10");
    assert_eq!(shown("c_ipv6"), "2001:db8::1");
    assert_eq!(get("c_enum"), Value::Text("b".into()));
    assert_eq!(get("c_array"), Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    assert_eq!(get("c_array_str"), Value::Array(vec![Value::Text("x".into()), Value::Null]));
    assert_eq!(shown("c_tuple"), "[7,\"seven\"]");
    assert_eq!(get("c_map"), Value::Json("{\"k1\":1,\"k2\":2}".into()));
    assert_eq!(get("c_json"), Value::Text("{\"a\": 1}".into()));

    // Insert row 3 from row 1's display texts (parsed back with parse_input), read it, compare, delete it.
    let mut values = vec![("id".to_owned(), Value::Int(3))];
    for (i, col) in page.columns.iter().enumerate().skip(1) {
        let v = &page.rows[0][i];
        let typed = match (v, &col.kind) {
            (Value::Array(items), _) => {
                let json: Vec<serde_json::Value> = items
                    .iter()
                    .map(|i| i.display().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
                    .collect();
                parse_input(&col.kind, &serde_json::to_string(&json).unwrap()).unwrap()
            }
            (Value::Other { .. }, _) if col.type_name.starts_with("Tuple") => Value::Other {
                type_name: String::new(),
                text: "(7,'seven')".into(),
            },
            (Value::Json(_), _) if col.type_name.starts_with("Map") => Value::Other {
                type_name: String::new(),
                text: "{'k1':1,'k2':2}".into(),
            },
            (other, kind) => parse_input(kind, &other.display().unwrap()).unwrap(),
        };
        values.push((col.name.clone(), typed));
    }
    let zoo = t("type_zoo");
    e.apply_changes(&zoo, vec![RowChange::Insert { values }]).await.unwrap();
    req.filter = vec![FilterCond::new("id", FilterOp::Eq, &["3"])];
    let again = e.browse(&req).await.unwrap();
    e.apply_changes(
        &zoo,
        vec![RowChange::Delete {
            key: vec![("id".into(), Value::Int(3))],
        }],
    )
    .await
    .unwrap();
    for (i, col) in again.columns.iter().enumerate().skip(1) {
        assert_eq!(again.rows[0][i], page.rows[0][i], "{}", col.name);
    }
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_row_changes() {
    let e = engine().await;
    let cats = t("categories");
    let s = e
        .apply_changes(
            &cats,
            vec![
                RowChange::Insert {
                    values: vec![("id".into(), Value::Int(100)), ("name".into(), Value::Text("Garden".into()))],
                },
                RowChange::Update {
                    key: vec![("id".into(), Value::Int(100))],
                    values: vec![("name".into(), Value::Text("Garden & Patio".into()))],
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((s.inserted, s.updated), (1, 1));
    let mut req = BrowseRequest::new(cats.clone(), 5);
    req.filter = vec![FilterCond::new("id", FilterOp::Eq, &["100"])];
    assert_eq!(e.browse(&req).await.unwrap().rows[0][1], Value::Text("Garden & Patio".into()));
    let err = e
        .apply_changes(
            &cats,
            vec![RowChange::Delete {
                key: vec![("id".into(), Value::Int(999))],
            }],
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("matched 0 rows"), "{err}");
    e.apply_changes(
        &cats,
        vec![RowChange::Delete {
            key: vec![("id".into(), Value::Int(100))],
        }],
    )
    .await
    .unwrap();
    assert!(e.browse(&req).await.unwrap().rows.is_empty());
}

#[tokio::test]
#[ignore]
async fn live_engine_clickhouse_streaming_limit_cancel_timeout() {
    let e = engine().await;
    let s = e.open_session().unwrap();
    assert_eq!(s.transaction_state(), TxState::Unsupported);
    let opts = RunOptions {
        read_only: false,
        row_limit: 1000,
        batch_size: 300,
        timeout: None,
    };

    // Several statements; numbers(1e10) stops at the limit and the query is gone from system.processes.
    let mut sink = CollectSink::default();
    s.run(
        "SELECT count() FROM shop.orders; SELECT number FROM system.numbers_mt LIMIT 10000000000; SELECT 1 FORMAT CSV",
        &opts,
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets.len(), 3);
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(4000));
    assert_eq!(sink.sets[1].rows.len(), 1000);
    assert!(sink.sets[1].truncated);
    assert_eq!(sink.sets[2].rows[0][0], Value::Text("1\n".into()));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_, procs) = e.http.query("SELECT count() FROM system.processes WHERE query LIKE '%numbers_mt LIMIT 10000000000%' AND query NOT LIKE '%system.processes%'", &[]).await.unwrap();
    assert_eq!(procs[0][0], Value::Int(0), "the truncated query was stopped");

    // SET carries over within the session.
    let mut sink = CollectSink::default();
    s.run("SET max_threads = 3; SELECT getSetting('max_threads')", &opts, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets[1].rows[0][0].display().unwrap(), "3");

    // Cancel a long aggregation with KILL QUERY.
    let cancel = s.canceller();
    let t0 = std::time::Instant::now();
    let c = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel.cancel().await.unwrap();
    });
    let err = s
        .run(
            "SELECT count() FROM numbers(10000000000) WHERE sipHash64(number) % 7 = 3",
            &opts,
            &mut CollectSink::default(),
        )
        .await
        .unwrap_err();
    c.await.unwrap();
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err}");
    assert!(t0.elapsed() < Duration::from_secs(10));

    // Timeout.
    let timed = RunOptions {
        read_only: false,
        timeout: Some(Duration::from_millis(500)),
        ..opts.clone()
    };
    let err = s
        .run(
            "SELECT count() FROM numbers(10000000000) WHERE sipHash64(number) % 7 = 3",
            &timed,
            &mut CollectSink::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout, "{err}");

    // INSERT reports written rows.
    let mut sink = CollectSink::default();
    s.run(
        "CREATE TEMPORARY TABLE tmp (x UInt8); INSERT INTO tmp SELECT number FROM numbers(5); SELECT count() FROM tmp",
        &opts,
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[1].affected, Some(5));
    assert_eq!(sink.sets[2].rows[0][0], Value::Int(5));
    assert_eq!(
        ChEngine::classify("ALTER TABLE shop.orders DROP PARTITION 202503").kind,
        StatementKind::Destructive
    );
}
