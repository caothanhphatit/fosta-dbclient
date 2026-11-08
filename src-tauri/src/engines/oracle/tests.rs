//! Live tests against the `oracle` service of dev/docker-compose.engines-sql.yml. Run with
//! `cargo test live_engine_ -- --ignored --test-threads=1`. They need Oracle Instant Client on PATH; without it each test checks
//! the "missing client" error, prints a note and passes.

use super::*;
use crate::engines::common::{parse_input, CollectSink, FilterCond, FilterOp, RunOptions, SortKey, StatementKind, TxState};

fn opts() -> OraOptions {
    OraOptions::parse("oracle://shop:studio-root@127.0.0.1:1521/FREEPDB1").unwrap()
}

/// Connects, or returns None (after checking the error) when Instant Client is not installed.
async fn engine() -> Option<OraEngine> {
    match OraEngine::connect(opts()).await {
        Ok(e) => Some(e),
        Err(e) if e.message.starts_with("Oracle Instant Client was not found") => {
            assert_eq!(e.kind, ErrorKind::Connect);
            assert!(e.message.contains(INSTANT_CLIENT_URL));
            eprintln!("note: Oracle Instant Client is not installed; skipping the Oracle live test ({e})");
            None
        }
        Err(e) => panic!("connect to the dev Oracle (docker compose -f dev/docker-compose.engines-sql.yml up -d oracle): {e}"),
    }
}

fn t(name: &str) -> ObjectRef {
    ObjectRef::new(None, Some("SHOP"), name, NodeKind::Table)
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_connect_and_version() {
    let Some(e) = engine().await else { return };
    assert!(e.server_version().starts_with("23"), "{}", e.server_version());
    assert_eq!(e.current_schema(), "SHOP");
    let mut bad = opts();
    bad.password = Some("wrong".into());
    let err = OraEngine::connect(bad).await.err().unwrap();
    assert_eq!((err.kind, err.code.as_deref()), (ErrorKind::Connect, Some("ORA-01017")), "{err}");
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_catalog_tree() {
    let Some(e) = engine().await else { return };
    let tree = e.catalog().await.unwrap();
    assert_eq!(tree[0].name, "SHOP", "the login schema comes first");
    let shop = &tree[0];
    for table in ["CUSTOMERS", "ORDERS", "ORDER_ITEMS", "PRODUCTS", "CATEGORIES", "TYPE_ZOO"] {
        assert!(shop.find(NodeKind::Table, table).is_some(), "{table}");
    }
    assert!(
        shop.find(NodeKind::Table, "SALES_BY_CATEGORY").is_none(),
        "the materialized view's container table is hidden"
    );
    assert!(shop.find(NodeKind::MaterializedView, "SALES_BY_CATEGORY").is_some());
    assert!(shop.find(NodeKind::View, "ORDER_TOTALS").is_some());
    assert!(shop.find(NodeKind::Sequence, "INVOICE_NO").is_some());
    assert!(shop.find(NodeKind::Package, "SHOP_PKG").is_some());
    assert!(shop.find(NodeKind::Procedure, "TOUCH_ORDER").is_some());
    assert!(shop.find(NodeKind::Function, "CUSTOMER_ORDER_COUNT").is_some());
    assert!(shop.find(NodeKind::Synonym, "CUST").is_some());
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_describe() {
    let Some(e) = engine().await else { return };
    let customers = e.describe(&t("CUSTOMERS")).await.unwrap();
    assert_eq!(customers.primary_key.as_ref().unwrap().columns, vec!["ID"]);
    assert!(customers.unique_keys.iter().any(|k| k.columns == vec!["EMAIL"]));
    assert_eq!(
        customers.column("EMAIL").unwrap().comment.as_deref(),
        Some("Login and contact address")
    );
    assert_eq!(customers.column("COUNTRY").unwrap().default.as_deref(), Some("'VN'"));
    assert_eq!(
        customers.column("ID").unwrap().kind,
        ValueKind::Decimal,
        "NUMBER(19) does not fit i64 in general"
    );
    assert_eq!(customers.column("CREATED_AT").unwrap().kind, ValueKind::TimestampTz);
    assert!(
        customers
            .indexes
            .iter()
            .any(|i| i.name == "CUSTOMERS_COUNTRY_IDX" && i.columns == vec!["COUNTRY", "SYS_EXTRACT_UTC(\"CREATED_AT\") DESC"]),
        "{:?}",
        customers.indexes
    );
    assert_eq!(customers.row_estimate, Some(2000));
    assert!(customers.ddl.as_deref().unwrap().contains("CREATE TABLE \"SHOP\".\"CUSTOMERS\""));

    let cats = e.describe(&t("CATEGORIES")).await.unwrap();
    assert_eq!(cats.column("ID").unwrap().identity.as_deref(), Some("ALWAYS"));
    assert_eq!(cats.comment.as_deref(), Some("Product categories"));
    let products = e.describe(&t("PRODUCTS")).await.unwrap();
    assert!(products.column("PRICE_WITH_VAT").unwrap().generated.is_some());
    let items = e.describe(&t("ORDER_ITEMS")).await.unwrap();
    assert_eq!(items.primary_key.unwrap().columns, vec!["ORDER_ID", "LINE_NO"]);
    let fk = items.foreign_keys.iter().find(|f| f.ref_table == "ORDERS").unwrap();
    assert_eq!(
        (fk.on_delete.as_deref(), fk.ref_columns.clone()),
        (Some("CASCADE"), vec!["ID".to_owned()])
    );

    let view = e
        .describe(&ObjectRef::new(None, Some("SHOP"), "ORDER_TOTALS", NodeKind::View))
        .await
        .unwrap();
    assert_eq!(view.object.kind, NodeKind::View);
    assert_eq!(view.columns.len(), 4);
    let mv = e
        .describe(&ObjectRef::new(None, Some("SHOP"), "SALES_BY_CATEGORY", NodeKind::Table))
        .await
        .unwrap();
    assert_eq!(mv.object.kind, NodeKind::MaterializedView);
    let pkg = e
        .describe(&ObjectRef::new(None, Some("SHOP"), "SHOP_PKG", NodeKind::Package))
        .await
        .unwrap();
    let ddl = pkg.ddl.unwrap();
    assert!(ddl.contains("PACKAGE") && ddl.contains("PACKAGE BODY"), "{ddl}");
    let seq = e
        .describe(&ObjectRef::new(None, Some("SHOP"), "INVOICE_NO", NodeKind::Sequence))
        .await
        .unwrap();
    assert!(seq.extra.contains(&("incrementBy".into(), "10".into())));
    let syn = e
        .describe(&ObjectRef::new(None, Some("SHOP"), "CUST", NodeKind::Synonym))
        .await
        .unwrap();
    assert!(syn.extra.contains(&("target".into(), "SHOP.CUSTOMERS".into())));
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_browse_filters_and_keyset() {
    let Some(e) = engine().await else { return };
    let mut req = BrowseRequest::new(t("ORDERS"), 100);
    req.filter = vec![
        FilterCond::new("STATUS", FilterOp::In, &["paid", "shipped"]),
        FilterCond::new("ORDERED_AT", FilterOp::Between, &["2025-03-02", "2025-03-20 12:00"]),
        FilterCond::new("NOTE", FilterOp::IsNull, &[]),
    ];
    req.sort = vec![SortKey::desc("ORDERED_AT")];
    req.estimate_total = true;
    let first = e.browse(&req).await.unwrap();
    assert_eq!(first.rows.len(), 100);
    assert!(
        first.sql.contains(":1") && first.sql.contains("FETCH NEXT 101 ROWS ONLY"),
        "{}",
        first.sql
    );
    assert!(first.estimated_total.unwrap() > 0);
    let mut next = req.clone();
    next.after = first.next_keyset.clone();
    let mut by_offset = req.clone();
    by_offset.offset = 100;
    assert_eq!(e.browse(&next).await.unwrap().rows, e.browse(&by_offset).await.unwrap().rows);
    let mut req = BrowseRequest::new(t("CUSTOMERS"), 10);
    req.filter = vec![
        FilterCond::new("EMAIL", FilterOp::Contains, &["CUSTOMER19"]),
        FilterCond::new("FULL_NAME", FilterOp::StartsWith, &["customer"]),
    ];
    assert!(!e.browse(&req).await.unwrap().rows.is_empty());
    req.filter = vec![FilterCond::new("FULL_NAME", FilterOp::Eq, &["x' OR '1'='1"])];
    assert!(e.browse(&req).await.unwrap().rows.is_empty());
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_typed_values_round_trip() {
    let Some(e) = engine().await else { return };
    let mut req = BrowseRequest::new(t("TYPE_ZOO"), 10);
    req.sort = vec![SortKey::asc("ID")];
    let page = e.browse(&req).await.unwrap();
    let get = |name: &str| page.rows[0][page.columns.iter().position(|c| c.name == name).unwrap()].clone();
    let shown = |name: &str| get(name).display().unwrap();
    assert_eq!(get("C_NUMBER"), Value::Decimal("3.14159".into()));
    assert_eq!(get("C_NUMBER_SCALE"), Value::Decimal("12345678901234.123456".into()));
    assert_eq!(get("C_INTEGER"), Value::Int(i64::MAX));
    assert_eq!(get("C_BINARY_FLOAT"), Value::Float(1.5));
    assert_eq!(get("C_BINARY_DOUBLE"), Value::Float(std::f64::consts::E));
    assert_eq!(get("C_FLOAT"), Value::Decimal("0.1".into()));
    assert_eq!(get("C_VARCHAR2"), Value::Text("text with 'quotes' and unicode: Xin chào".into()));
    assert_eq!(get("C_NVARCHAR2"), Value::Text("Xin chào".into()));
    assert_eq!(get("C_CHAR"), Value::Text("ab  ".into()));
    assert_eq!(get("C_CLOB"), Value::Text("a CLOB value".into()));
    assert_eq!(get("C_NCLOB"), Value::Text("an NCLOB value".into()));
    assert_eq!(get("C_BLOB"), Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(shown("C_RAW"), "0xa0eebc999c0b4ef8bb6d6bb9bd380a11");
    assert_eq!(shown("C_DATE"), "2026-02-28 13:45:30");
    assert_eq!(shown("C_TIMESTAMP"), "2026-02-28 13:45:30.123456");
    assert_eq!(shown("C_TIMESTAMP_TZ"), "2026-02-28 13:45:30.500+07:00");
    assert_eq!(shown("C_INTERVAL_YM"), "+01-02");
    assert_eq!(shown("C_INTERVAL_DS"), "+03 04:05:06.500000");
    assert_eq!(get("C_BOOLEAN"), Value::Bool(true));
    assert_eq!(get("C_JSON"), Value::Json("{\"a\":1,\"b\":[true,null]}".into()));
    assert!(page.rows[1][1..].iter().all(Value::is_null), "row 2 is all NULL");

    let mut values = Vec::new();
    for (i, col) in page.columns.iter().enumerate().skip(1) {
        values.push((
            col.name.clone(),
            parse_input(&col.kind, &page.rows[0][i].display().unwrap()).unwrap(),
        ));
    }
    let zoo = t("TYPE_ZOO");
    e.apply_changes(
        &zoo,
        vec![RowChange::Update {
            key: vec![("ID".into(), Value::Int(2))],
            values: values.clone(),
        }],
    )
    .await
    .unwrap();
    let again = e.browse(&req).await.unwrap();
    let restore = values.iter().map(|(n, _)| (n.clone(), Value::Null)).collect();
    e.apply_changes(
        &zoo,
        vec![RowChange::Update {
            key: vec![("ID".into(), Value::Int(2))],
            values: restore,
        }],
    )
    .await
    .unwrap();
    for (i, col) in again.columns.iter().enumerate().skip(1) {
        assert_eq!(again.rows[1][i], again.rows[0][i], "{}", col.name);
    }
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_row_changes_in_one_transaction() {
    let Some(e) = engine().await else { return };
    let cats = t("CATEGORIES");
    let s = e
        .apply_changes(
            &cats,
            vec![
                RowChange::Insert {
                    values: vec![("NAME".into(), Value::Text("Garden".into()))],
                },
                RowChange::Update {
                    key: vec![("ID".into(), Value::Int(6))],
                    values: vec![("NAME".into(), Value::Text("Fiction".into()))],
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((s.inserted, s.updated), (1, 1));
    let err = e
        .apply_changes(
            &cats,
            vec![
                RowChange::Update {
                    key: vec![("ID".into(), Value::Int(6))],
                    values: vec![("NAME".into(), Value::Text("Novels".into()))],
                },
                RowChange::Delete {
                    key: vec![("ID".into(), Value::Int(999))],
                },
            ],
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("matched 0 rows"), "{err}");
    let mut req = BrowseRequest::new(cats.clone(), 5);
    req.filter = vec![FilterCond::new("ID", FilterOp::Eq, &["6"])];
    assert_eq!(
        e.browse(&req).await.unwrap().rows[0][1],
        Value::Text("Fiction".into()),
        "rolled back"
    );
    let fk = e
        .apply_changes(
            &cats,
            vec![RowChange::Delete {
                key: vec![("ID".into(), Value::Int(1))],
            }],
        )
        .await
        .unwrap_err();
    assert_eq!(fk.code.as_deref(), Some("ORA-02292"), "{fk}");
    // Restore.
    req.filter = vec![FilterCond::new("NAME", FilterOp::Eq, &["Garden"])];
    let garden = e.browse(&req).await.unwrap().rows[0][0].clone();
    e.apply_changes(
        &cats,
        vec![
            RowChange::Delete {
                key: vec![("ID".into(), garden)],
            },
            RowChange::Update {
                key: vec![("ID".into(), Value::Int(6))],
                values: vec![("NAME".into(), Value::Text("Novels".into()))],
            },
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_streaming_limit_cancel_timeout() {
    let Some(e) = engine().await else { return };
    let s = e.open_session().await.unwrap();
    let opts = RunOptions {
        read_only: false,
        row_limit: 1000,
        batch_size: 250,
        timeout: None,
    };
    let script = "SELECT COUNT(*) FROM orders;\nSELECT LEVEL FROM dual CONNECT BY LEVEL <= 100000000\n/\n\
                  DECLARE c SYS_REFCURSOR; BEGIN OPEN c FOR SELECT 'x' AS v FROM dual; DBMS_SQL.RETURN_RESULT(c); END;\n/\n";
    let t0 = std::time::Instant::now();
    let mut sink = CollectSink::default();
    s.run(script, &opts, &mut sink).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(4000));
    assert_eq!(sink.sets[1].rows.len(), 1000);
    assert!(sink.sets[1].truncated);
    assert_eq!(sink.sets[3].rows[0][0], Value::Text("x".into()), "implicit result set");

    let slow = "SELECT COUNT(*) FROM dual CONNECT BY LEVEL <= 1000000000";
    let cancel = s.canceller();
    let c = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        tokio::task::spawn_blocking(move || cancel.cancel()).await.unwrap().unwrap();
    });
    let err = s.run(slow, &opts, &mut CollectSink::default()).await.unwrap_err();
    c.await.unwrap();
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err}");
    let timed = RunOptions {
        read_only: false,
        timeout: Some(Duration::from_millis(500)),
        ..opts.clone()
    };
    let err = s.run(slow, &timed, &mut CollectSink::default()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout, "{err}");
    let mut sink = CollectSink::default();
    s.run("SELECT 1 FROM dual", &opts, &mut sink).await.unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(1));
    assert_eq!(OraEngine::classify("BEGIN NULL; END;").kind, StatementKind::Write);
}

#[tokio::test]
#[ignore]
async fn live_engine_oracle_transaction_state() {
    let Some(e) = engine().await else { return };
    let s = e.open_session().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
    let mut sink = CollectSink::default();
    s.run("UPDATE orders SET note = 'x' WHERE id = 1", &RunOptions::default(), &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].affected, Some(1));
    assert_eq!(s.transaction_state().await, TxState::Active);
    s.rollback().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
    s.run(
        "UPDATE orders SET note = note WHERE id = 1",
        &RunOptions::default(),
        &mut CollectSink::default(),
    )
    .await
    .unwrap();
    s.commit().await.unwrap();
    assert_eq!(s.transaction_state().await, TxState::Idle);
}
