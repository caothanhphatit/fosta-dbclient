//! Live tests against dev/seed/shop.sqlite (built from dev/seed/sqlite.sql when missing). Run with
//! `cargo test live_engine_ -- --ignored --test-threads=1`.

use super::*;
use crate::engines::common::{
    parse_input, CollectSink, FilterCond, FilterOp, Flow, RunOptions, SortKey, StatementKind, StreamEvent, TxState,
};
use std::path::Path;

fn seed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("dev").join("seed")
}

/// Builds shop.sqlite from the seed script once; returns its path.
fn shop_db() -> PathBuf {
    let path = seed_dir().join("shop.sqlite");
    if !path.exists() {
        let script = std::fs::read_to_string(seed_dir().join("sqlite.sql")).expect("dev/seed/sqlite.sql");
        let tmp = seed_dir().join(format!("shop.sqlite.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let conn = Connection::open(&tmp).expect("create seed db");
        conn.execute_batch(&script).expect("seed script");
        drop(conn);
        std::fs::rename(&tmp, &path).expect("move seed db into place");
    }
    path
}

/// A private copy for tests that change data.
fn scratch_copy(name: &str) -> PathBuf {
    let dst = std::env::temp_dir().join(format!("fosta-live-{name}-{}.sqlite", std::process::id()));
    std::fs::copy(shop_db(), &dst).expect("copy seed db");
    dst
}

async fn engine() -> SqliteEngine {
    let mut o = SqliteOptions::file(shop_db().to_str().unwrap());
    o.read_only = true;
    SqliteEngine::connect(o).await.expect("connect")
}

fn t(name: &str) -> ObjectRef {
    ObjectRef::new(None, Some("main"), name, NodeKind::Table)
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_connect_and_version() {
    let e = engine().await;
    assert!(e.server_version().starts_with('3'), "{}", e.server_version());
    let missing = SqliteEngine::connect(SqliteOptions::file("Z:/no/such/file.db")).await;
    assert!(missing.is_err());
    let uri = format!("sqlite:///{}?mode=ro", shop_db().to_str().unwrap().replace('\\', "/"));
    SqliteEngine::connect(SqliteOptions::from_uri(&uri).unwrap())
        .await
        .expect("connect by URI");
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_catalog_tree() {
    let e = engine().await;
    let tree = e.catalog().await.unwrap();
    let main = tree.iter().find(|n| n.name == "main").expect("main database");
    for table in [
        "customers",
        "orders",
        "order_items",
        "products",
        "categories",
        "type_zoo",
        "audit_log",
    ] {
        assert!(main.find(NodeKind::Table, table).is_some(), "{table}");
    }
    assert!(main.find(NodeKind::View, "order_totals").is_some());
    assert!(main.find(NodeKind::Index, "orders_customer_idx").is_some());
    assert!(main.find(NodeKind::Trigger, "orders_audit").is_some());
    assert!(main.find(NodeKind::Table, "sqlite_stat1").is_none(), "internal tables are hidden");
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_describe() {
    let e = engine().await;
    let orders = e.describe(&t("orders")).await.unwrap();
    assert_eq!(orders.primary_key.as_ref().unwrap().columns, vec!["id"]);
    assert_eq!(orders.column("id").unwrap().identity.as_deref(), Some("ROWID"));
    let fk = &orders.foreign_keys[0];
    assert_eq!(
        (fk.ref_table.as_str(), fk.columns[0].as_str(), fk.on_delete.as_deref()),
        ("customers", "customer_id", Some("CASCADE"))
    );
    assert!(orders
        .indexes
        .iter()
        .any(|i| i.name == "orders_customer_idx" && i.columns == vec!["customer_id"]));
    assert_eq!(orders.row_estimate, Some(4000));
    let ddl = orders.ddl.unwrap();
    assert!(
        ddl.starts_with("CREATE TABLE orders") && ddl.contains("CREATE TRIGGER orders_audit"),
        "{ddl}"
    );

    let items = e.describe(&t("order_items")).await.unwrap();
    assert_eq!(items.primary_key.unwrap().columns, vec!["order_id", "line_no"]);
    assert!(items.extra.contains(&("withoutRowid".into(), "true".into())));
    assert_eq!(items.foreign_keys.len(), 2);

    let customers = e.describe(&t("customers")).await.unwrap();
    assert!(customers.unique_keys.iter().any(|k| k.columns == vec!["email"]));
    assert_eq!(customers.column("country").unwrap().default.as_deref(), Some("'VN'"));
    assert!(!customers.column("email").unwrap().nullable);
    assert!(customers.indexes.iter().any(|i| i.columns == vec!["country", "created_at DESC"]));

    let products = e.describe(&t("products")).await.unwrap();
    assert_eq!(products.column("price_with_vat").unwrap().generated.as_deref(), Some("VIRTUAL"));
    assert!(products.indexes.iter().any(|i| i.columns == vec!["<expression>"]));
    let categories = e.describe(&t("categories")).await.unwrap();
    assert_eq!(categories.column("id").unwrap().identity.as_deref(), Some("AUTOINCREMENT"));

    let view = e
        .describe(&ObjectRef::new(None, Some("main"), "order_totals", NodeKind::View))
        .await
        .unwrap();
    assert_eq!(view.object.kind, NodeKind::View);
    assert_eq!(view.columns.len(), 4);
    assert!(view.ddl.unwrap().starts_with("CREATE VIEW"));
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_browse_filters_and_keyset() {
    let e = engine().await;
    let mut req = BrowseRequest::new(t("customers"), 25);
    req.filter = vec![
        FilterCond::new("country", FilterOp::Eq, &["DE"]),
        FilterCond::new("email", FilterOp::Contains, &["CUSTOMER1"]),
        FilterCond::new("vip", FilterOp::Eq, &["false"]),
    ];
    req.estimate_total = true;
    let page = e.browse(&req).await.unwrap();
    assert!(!page.rows.is_empty());
    let country = page.columns.iter().position(|c| c.name == "country").unwrap();
    assert!(page.rows.iter().all(|r| r[country] == Value::Text("DE".into())));
    assert!(page.sql.contains("?1") && !page.sql.contains("'DE'"));
    assert_eq!(page.key_columns, vec!["id"]);

    // A quote in a filter value is data, not SQL.
    let mut inj = BrowseRequest::new(t("customers"), 5);
    inj.filter = vec![FilterCond::new("full_name", FilterOp::Eq, &["x' OR '1'='1"])];
    assert!(e.browse(&inj).await.unwrap().rows.is_empty());

    // Keyset paging over orders by ordered_at DESC: the pages meet without gaps or overlaps.
    let mut req = BrowseRequest::new(t("orders"), 100);
    req.sort = vec![SortKey::desc("ordered_at")];
    req.filter = vec![FilterCond::new("status", FilterOp::In, &["paid", "shipped"])];
    let first = e.browse(&req).await.unwrap();
    assert!(first.has_more);
    let cursor = first.next_keyset.clone().expect("keyset cursor");
    req.after = Some(cursor);
    let second = e.browse(&req).await.unwrap();
    req.after = None;
    req.offset = 100;
    let by_offset = e.browse(&req).await.unwrap();
    assert_eq!(second.rows, by_offset.rows);

    // Rowid table without a primary key gets the rowid pseudo-column as its key.
    let log = e.browse(&BrowseRequest::new(t("audit_log"), 5)).await.unwrap();
    assert_eq!(log.key_columns, vec![ROWID]);
    let mut r = BrowseRequest::new(t("order_items"), 1);
    r.estimate_total = true;
    let total = e.browse(&r);
    assert!(total.await.unwrap().estimated_total.unwrap() > 7000);
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_typed_values_round_trip() {
    let path = scratch_copy("types");
    let e = SqliteEngine::connect(SqliteOptions::file(path.to_str().unwrap())).await.unwrap();
    let mut req = BrowseRequest::new(t("type_zoo"), 10);
    req.sort = vec![SortKey::asc("id")];
    let page = e.browse(&req).await.unwrap();
    let get = |name: &str| page.rows[0][page.columns.iter().position(|c| c.name == name).unwrap()].clone();
    assert_eq!(get("c_integer"), Value::Int(2147483647));
    assert_eq!(get("c_bigint"), Value::Int(i64::MAX));
    assert_eq!(get("c_real"), Value::Float(1.5));
    assert_eq!(get("c_numeric"), Value::Int(42));
    assert_eq!(get("c_decimal"), Value::Decimal("12345.678901".into()));
    assert_eq!(get("c_boolean"), Value::Bool(true));
    assert_eq!(get("c_text"), Value::Text("text with 'quotes' and unicode: Xin chào".into()));
    assert_eq!(get("c_blob"), Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(get("c_date").display().unwrap(), "2026-02-28");
    assert_eq!(get("c_time").display().unwrap(), "13:45:30.123");
    assert!(matches!(get("c_datetime"), Value::Timestamp(_)));
    assert_eq!(get("c_timestamp").display().unwrap(), "2026-02-28 13:45:30.500");
    assert!(matches!(get("c_json"), Value::Json(_)));
    assert_eq!(get("c_uuid"), Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()));
    assert_eq!(get("c_untyped"), Value::Text("anything".into()));
    assert!(page.rows[1][1..].iter().all(Value::is_null), "row 2 is all NULL");

    // Write every display value back through parse_input into row 2, then read it again: the same typed values.
    let desc = e.describe(&t("type_zoo")).await.unwrap();
    let mut values = Vec::new();
    for (i, col) in page.columns.iter().enumerate().skip(1) {
        let v = &page.rows[0][i];
        let typed = parse_input(&col.kind, &v.display().unwrap()).unwrap();
        assert!(desc.column(&col.name).is_some());
        values.push((col.name.clone(), typed));
    }
    e.apply_changes(
        &t("type_zoo"),
        vec![RowChange::Update {
            key: vec![("id".into(), Value::Int(2))],
            values,
        }],
    )
    .await
    .unwrap();
    let again = e.browse(&req).await.unwrap();
    assert_eq!(again.rows[1][1..], again.rows[0][1..]);
    drop(e);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_row_changes_in_one_transaction() {
    let path = scratch_copy("changes");
    let e = SqliteEngine::connect(SqliteOptions::file(path.to_str().unwrap())).await.unwrap();
    let cats = t("categories");
    e.apply_changes(
        &cats,
        vec![RowChange::Insert {
            values: vec![("name".into(), Value::Text("Temp".into()))],
        }],
    )
    .await
    .unwrap();
    let s = e
        .apply_changes(
            &cats,
            vec![
                RowChange::Insert {
                    values: vec![("name".into(), Value::Text("Garden".into())), ("parent_id".into(), Value::Int(3))],
                },
                RowChange::Update {
                    key: vec![("id".into(), Value::Int(6))],
                    values: vec![("name".into(), Value::Text("Fiction".into()))],
                },
                RowChange::Delete {
                    key: vec![("id".into(), Value::Int(8))],
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((s.inserted, s.updated, s.deleted), (1, 1, 1));
    let page = e.browse(&BrowseRequest::new(cats.clone(), 50)).await.unwrap();
    let names: Vec<String> = page.rows.iter().map(|r| r[1].display().unwrap()).collect();
    assert!(names.contains(&"Garden".to_owned()) && names.contains(&"Fiction".to_owned()) && !names.contains(&"Temp".to_owned()));

    // A failing change rolls back the whole batch.
    let err = e
        .apply_changes(
            &cats,
            vec![
                RowChange::Update {
                    key: vec![("id".into(), Value::Int(1))],
                    values: vec![("name".into(), Value::Text("Gadgets".into()))],
                },
                RowChange::Delete {
                    key: vec![("id".into(), Value::Int(999))],
                },
            ],
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("matched 0 rows"), "{err}");
    let after = e.browse(&BrowseRequest::new(cats.clone(), 50)).await.unwrap();
    assert_eq!(after.rows[0][1], Value::Text("Electronics".into()));

    // A foreign key violation is reported by the server and rolls back too.
    let fk = e
        .apply_changes(
            &cats,
            vec![RowChange::Delete {
                key: vec![("id".into(), Value::Int(1))],
            }],
        )
        .await
        .unwrap_err();
    assert!(fk.message.contains("FOREIGN KEY"), "{fk}");

    // Rowid tables without a key are edited by rowid.
    let log = t("audit_log");
    e.apply_changes(
        &log,
        vec![RowChange::Insert {
            values: vec![("message".into(), Value::Text("hello".into()))],
        }],
    )
    .await
    .unwrap();
    let page = e.browse(&BrowseRequest::new(log.clone(), 5)).await.unwrap();
    let rowid = page.rows[0][0].clone();
    e.apply_changes(
        &log,
        vec![RowChange::Delete {
            key: vec![(ROWID.into(), rowid)],
        }],
    )
    .await
    .unwrap();
    assert!(e.browse(&BrowseRequest::new(log, 5)).await.unwrap().rows.is_empty());
    drop(e);
    let _ = std::fs::remove_file(path);
}

const ENDLESS: &str = "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) SELECT x FROM n";

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_streaming_limit_cancel_timeout() {
    let e = engine().await;
    let s = e.open_session().await.unwrap();

    // Several result sets, batches and the row limit on an endless query.
    let opts = RunOptions {
        row_limit: 1000,
        batch_size: 100,
        timeout: None,
    };
    let mut sink = CollectSink::default();
    s.run(&format!("SELECT count(*) FROM orders; {ENDLESS};"), &opts, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets.len(), 2);
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(4000));
    assert_eq!(sink.sets[1].rows.len(), 1000);
    assert!(sink.sets[1].truncated);
    assert!(sink.batches >= 11);

    // The consumer can stop early.
    let mut seen = 0;
    let mut stopper = |ev: StreamEvent| {
        if let StreamEvent::Rows { rows, .. } = ev {
            seen += rows.len();
            return Flow::Stop;
        }
        Flow::Continue
    };
    s.run(ENDLESS, &opts, &mut stopper).await.unwrap();
    // Batches already in flight still arrive, but reading stops long before the limit.
    assert!((100..=600).contains(&seen), "{seen}");

    // Cancel a long statement from another task.
    let cancel = s.canceller();
    let long = "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 1000000000) SELECT count(*) FROM n";
    let t0 = std::time::Instant::now();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let err = s.run(long, &opts, &mut CollectSink::default()).await.unwrap_err();
    canceller.await.unwrap();
    assert_eq!(err.kind, crate::engines::common::ErrorKind::Cancelled, "{err}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));

    // Timeout.
    let opts = RunOptions {
        timeout: Some(std::time::Duration::from_millis(300)),
        ..RunOptions::default()
    };
    let err = s.run(long, &opts, &mut CollectSink::default()).await.unwrap_err();
    assert_eq!(err.kind, crate::engines::common::ErrorKind::Timeout, "{err}");

    // The session still works afterwards.
    let mut sink = CollectSink::default();
    s.run("select 1", &RunOptions::default(), &mut sink).await.unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(1));
}

#[tokio::test]
#[ignore]
async fn live_engine_sqlite_transaction_state() {
    let path = scratch_copy("tx");
    let e = SqliteEngine::connect(SqliteOptions::file(path.to_str().unwrap())).await.unwrap();
    let s = e.open_session().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
    let mut sink = CollectSink::default();
    s.run(
        "BEGIN; UPDATE orders SET status = 'paid' WHERE id = 4",
        &RunOptions::default(),
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[1].affected, Some(1));
    assert_eq!(s.transaction_state().await, TxState::Active);
    // The trigger wrote to audit_log inside the same transaction.
    let mut sink = CollectSink::default();
    s.run("SELECT count(*) FROM audit_log", &RunOptions::default(), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(1));
    s.rollback().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
    let mut sink = CollectSink::default();
    s.run("SELECT status FROM orders WHERE id = 4", &RunOptions::default(), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Text("new".into()));
    s.begin().await.unwrap();
    s.run("DELETE FROM audit_log", &RunOptions::default(), &mut CollectSink::default())
        .await
        .unwrap();
    s.commit().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
    // INSERT … RETURNING reports rows and the change.
    let mut sink = CollectSink::default();
    s.run(
        "INSERT INTO audit_log (message) VALUES ('a'), ('b') RETURNING message",
        &RunOptions::default(),
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[0].rows.len(), 2);
    assert_eq!(sink.sets[0].affected, Some(2));
    assert_eq!(SqliteEngine::classify("DELETE FROM audit_log").kind, StatementKind::Destructive);
    drop((s, e));
    let _ = std::fs::remove_file(path);
}
