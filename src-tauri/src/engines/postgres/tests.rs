//! Live tests against the `postgres` service of dev/compose/{postgres,clickhouse,oracle}.yml. Run with
//! `cargo test live_engine_ -- --ignored --test-threads=1`.

use super::*;
use crate::engines::common::{
    parse_input, CatalogNode, CollectSink, FilterCond, FilterOp, NodeKind, RunOptions, SortKey, StatementKind, TxState, Value, ValueKind,
};

fn opts() -> PgOptions {
    PgOptions::parse("postgres://postgres:studio-root@127.0.0.1:5432/shop").unwrap()
}

async fn engine() -> PgEngine {
    PgEngine::connect(opts())
        .await
        .expect("connect to the dev PostgreSQL (docker compose -f dev/compose/{postgres,clickhouse,oracle}.yml up -d)")
}

fn t(schema: &str, name: &str) -> ObjectRef {
    ObjectRef::new(None, Some(schema), name, NodeKind::Table)
}

fn folder<'a>(schema: &'a CatalogNode, label: &str) -> &'a CatalogNode {
    schema.children.iter().find(|c| c.name == label).expect(label)
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_connect_and_version() {
    let e = engine().await;
    assert!(e.server_version().starts_with("17"), "{}", e.server_version());
    assert!(e.list_databases().await.unwrap().contains(&"shop".to_owned()));
    let other = e.connect_database("postgres").await.unwrap();
    assert_eq!(other.database(), "postgres");
    let mut bad = opts();
    bad.password = Some("wrong".into());
    let err = PgEngine::connect(bad).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::Connect, "{err}");
    let mut closed = opts();
    closed.port = 1;
    closed.connect_timeout = Duration::from_secs(2);
    assert!(PgEngine::connect(closed).await.is_err());
    // The server has no TLS: requiring it fails with a clear error instead of falling back.
    let mut tls = opts();
    tls.tls = TlsMode::SkipVerify;
    assert!(PgEngine::connect(tls).await.is_err());
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_catalog_tree() {
    let e = engine().await;
    let tree = e.catalog().await.unwrap();
    let public = tree
        .children
        .iter()
        .find(|n| n.kind == NodeKind::Schema && n.name == "public")
        .expect("public");
    for table in ["customers", "orders", "order_items", "products", "categories", "type_zoo"] {
        assert!(folder(public, "Tables").find(NodeKind::Table, table).is_some(), "{table}");
    }
    assert!(folder(public, "Views").find(NodeKind::View, "order_totals").is_some());
    assert!(folder(public, "Materialized Views")
        .find(NodeKind::MaterializedView, "sales_by_category")
        .is_some());
    assert!(folder(public, "Sequences").find(NodeKind::Sequence, "invoice_no").is_some());
    assert!(folder(public, "Functions")
        .find(NodeKind::Function, "customer_order_count")
        .is_some());
    assert_eq!(
        folder(public, "Functions").count(NodeKind::Function),
        1,
        "pgcrypto's functions are hidden"
    );
    let types = folder(public, "Types");
    for ty in ["order_status", "positive_money", "address"] {
        assert!(types.find(NodeKind::Type, ty).is_some(), "{ty}");
    }
    let sales = tree.children.iter().find(|n| n.name == "sales").expect("sales schema");
    assert!(sales.find(NodeKind::Table, "targets").is_some());
    let exts = tree.children.iter().find(|n| n.name == "Extensions").unwrap();
    assert!(exts.find(NodeKind::Extension, "pgcrypto").is_some());
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_describe() {
    let e = engine().await;
    let customers = e.describe(&t("public", "customers")).await.unwrap();
    assert_eq!(customers.primary_key.as_ref().unwrap().columns, vec!["id"]);
    assert_eq!(customers.column("id").unwrap().identity.as_deref(), Some("serial"));
    assert_eq!(
        customers.column("email").unwrap().comment.as_deref(),
        Some("Login and contact address")
    );
    assert_eq!(customers.column("email").unwrap().type_name, "character varying(200)");
    assert!(customers.unique_keys.iter().any(|k| k.columns == vec!["email"]));
    assert!(customers
        .indexes
        .iter()
        .any(|i| i.name == "customers_country_idx" && i.columns == vec!["country", "created_at DESC"]));
    assert!(
        (1500..2500).contains(&customers.row_estimate.unwrap()),
        "{:?}",
        customers.row_estimate
    );

    let cats = e.describe(&t("public", "categories")).await.unwrap();
    assert_eq!(cats.column("id").unwrap().identity.as_deref(), Some("ALWAYS"));
    assert_eq!(cats.comment.as_deref(), Some("Product categories, optionally nested"));
    assert_eq!(cats.foreign_keys[0].ref_table, "categories");

    let products = e.describe(&t("public", "products")).await.unwrap();
    let vat = products.column("price_with_vat").unwrap();
    assert!(vat.generated.as_deref().unwrap().contains("price"), "{vat:?}");
    assert_eq!(products.column("price").unwrap().kind, ValueKind::Decimal, "domain over numeric");
    assert_eq!(products.column("tags").unwrap().kind, ValueKind::Array(Box::new(ValueKind::Text)));
    assert!(products.indexes.iter().any(|i| i.columns == vec!["lower(name)"]));

    let items = e.describe(&t("public", "order_items")).await.unwrap();
    assert_eq!(items.primary_key.unwrap().columns, vec!["order_id", "line_no"]);
    let fk = items.foreign_keys.iter().find(|f| f.ref_table == "orders").unwrap();
    assert_eq!(fk.on_delete.as_deref(), Some("CASCADE"));
    let ddl = items.ddl.unwrap();
    assert!(ddl.starts_with("CREATE TABLE public.order_items ("), "{ddl}");
    assert!(
        ddl.contains("CHECK (quantity > 0)") && ddl.contains("REFERENCES orders(id) ON DELETE CASCADE"),
        "{ddl}"
    );

    let orders_ddl = e.describe(&t("public", "orders")).await.unwrap().ddl.unwrap();
    assert!(orders_ddl.contains("CREATE INDEX orders_customer_idx"), "{orders_ddl}");
    assert!(
        orders_ddl.contains("status order_status DEFAULT 'new'::order_status NOT NULL"),
        "{orders_ddl}"
    );

    let view = e
        .describe(&ObjectRef::new(None, Some("public"), "order_totals", NodeKind::View))
        .await
        .unwrap();
    assert_eq!(view.object.kind, NodeKind::View);
    assert!(view.ddl.unwrap().starts_with("CREATE OR REPLACE VIEW public.order_totals AS"));
    let mv = e
        .describe(&ObjectRef::new(None, Some("public"), "sales_by_category", NodeKind::View))
        .await
        .unwrap();
    assert_eq!(mv.object.kind, NodeKind::MaterializedView);

    let f = e
        .describe(&ObjectRef::new(None, Some("public"), "customer_order_count", NodeKind::Function))
        .await
        .unwrap();
    assert!(f
        .ddl
        .unwrap()
        .contains("CREATE OR REPLACE FUNCTION public.customer_order_count(cid bigint)"));
    let seq = e
        .describe(&ObjectRef::new(None, Some("public"), "invoice_no", NodeKind::Sequence))
        .await
        .unwrap();
    assert!(seq.ddl.unwrap().contains("INCREMENT BY 10"));
    let en = e
        .describe(&ObjectRef::new(None, Some("public"), "order_status", NodeKind::Type))
        .await
        .unwrap();
    assert_eq!(
        en.ddl.unwrap(),
        "CREATE TYPE public.order_status AS ENUM ('new', 'paid', 'shipped', 'cancelled');"
    );
    let dom = e
        .describe(&ObjectRef::new(None, Some("public"), "positive_money", NodeKind::Type))
        .await
        .unwrap();
    assert!(dom.ddl.unwrap().contains("CHECK (VALUE >= 0"));
    let comp = e
        .describe(&ObjectRef::new(None, Some("public"), "address", NodeKind::Type))
        .await
        .unwrap();
    assert_eq!(comp.columns.len(), 3);
    let ext = e
        .describe(&ObjectRef::new(None, None, "pgcrypto", NodeKind::Extension))
        .await
        .unwrap();
    assert!(ext.ddl.unwrap().starts_with("CREATE EXTENSION IF NOT EXISTS pgcrypto"));
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_browse_filters_and_keyset() {
    let e = engine().await;
    let mut req = BrowseRequest::new(t("public", "orders"), 50);
    req.filter = vec![
        FilterCond::new("status", FilterOp::In, &["paid", "shipped"]),
        FilterCond::new("ordered_at", FilterOp::Between, &["2025-03-02", "2025-03-20 12:00"]),
        FilterCond::new("note", FilterOp::IsNull, &[]),
    ];
    req.sort = vec![SortKey::desc("ordered_at")];
    req.estimate_total = true;
    let first = e.browse(&req).await.unwrap();
    assert_eq!(first.rows.len(), 50);
    assert!(first.has_more);
    assert!(first.sql.contains("($1::text)::order_status"), "{}", first.sql);
    assert!(first.estimated_total.unwrap() > 100, "{:?}", first.estimated_total);
    let status = first.columns.iter().position(|c| c.name == "status").unwrap();
    assert!(first
        .rows
        .iter()
        .all(|r| matches!(&r[status], Value::Text(s) if s == "paid" || s == "shipped")));

    let mut next = req.clone();
    next.after = first.next_keyset.clone();
    let by_key = e.browse(&next).await.unwrap();
    let mut by_offset = req.clone();
    by_offset.offset = 50;
    assert_eq!(by_key.rows, e.browse(&by_offset).await.unwrap().rows);

    // Contains on a non-text column, injection attempt, array and enum filters.
    let mut req = BrowseRequest::new(t("public", "products"), 10);
    req.filter = vec![
        FilterCond::new("sku", FilterOp::Contains, &["sku-0001"]),
        FilterCond::new("price", FilterOp::Gt, &["0"]),
    ];
    let p = e.browse(&req).await.unwrap();
    assert_eq!(p.rows.len(), 10);
    req.filter = vec![FilterCond::new("name", FilterOp::Eq, &["x'; DROP TABLE products; --"])];
    assert!(e.browse(&req).await.unwrap().rows.is_empty());
    req.filter = vec![FilterCond::new("tags", FilterOp::Eq, &[r#"["tag1", "tag1"]"#])];
    let tagged = e.browse(&req).await.unwrap();
    assert!(!tagged.rows.is_empty());
    let page = e.browse(&BrowseRequest::new(t("sales", "targets"), 10)).await.unwrap();
    assert_eq!(page.key_columns, vec!["year", "region"]);
    assert_eq!(page.rows.len(), 3);
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_typed_values_round_trip() {
    let e = engine().await;
    let mut req = BrowseRequest::new(t("public", "type_zoo"), 10);
    req.sort = vec![SortKey::asc("id")];
    let page = e.browse(&req).await.unwrap();
    let get = |name: &str| page.rows[0][page.columns.iter().position(|c| c.name == name).unwrap()].clone();
    let shown = |name: &str| get(name).display().unwrap();
    assert_eq!(get("c_bool"), Value::Bool(true));
    assert_eq!(get("c_int2"), Value::Int(-32768));
    assert_eq!(get("c_int8"), Value::Int(i64::MAX));
    assert_eq!(get("c_float4"), Value::Float(1.5));
    assert_eq!(get("c_numeric"), Value::Decimal("12345678901234.123456".into()));
    assert_eq!(get("c_money"), Value::Decimal("19.99".into()));
    assert_eq!(get("c_char"), Value::Text("ab  ".into()));
    assert_eq!(get("c_text"), Value::Text("text with 'quotes' and unicode: Xin chào".into()));
    assert_eq!(get("c_bytea"), Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(shown("c_date"), "2026-02-28");
    assert_eq!(shown("c_time"), "13:45:30.123456");
    assert_eq!(shown("c_timetz"), "13:45:30+07");
    assert_eq!(shown("c_timestamp"), "2026-02-28 13:45:30.500");
    assert_eq!(shown("c_timestamptz"), "2026-02-28 06:45:30.500+00:00");
    assert_eq!(get("c_interval"), Value::Interval("1 year 2 mons 3 days 04:05:06".into()));
    assert_eq!(get("c_uuid"), Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()));
    assert_eq!(get("c_json"), Value::Json(r#"{"a": 1, "b": [true, null]}"#.into()));
    assert_eq!(get("c_jsonb"), Value::Json(r#"{"nested": {"x": 1.5}}"#.into()));
    assert_eq!(get("c_int_array"), Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    assert_eq!(
        get("c_text_array"),
        Value::Array(vec![Value::Text("x".into()), Value::Text("y z".into()), Value::Null])
    );
    assert_eq!(shown("c_inet"), "192.168.1.10");
    assert_eq!(shown("c_cidr"), "10.0.0.0/8");
    assert_eq!(shown("c_macaddr"), "08:00:2b:01:02:03");
    assert_eq!(shown("c_bit"), "10101010");
    assert_eq!(shown("c_varbit"), "101");
    assert_eq!(shown("c_point"), "(1.5,-2)");
    assert_eq!(shown("c_range"), "[1,10)");
    assert_eq!(shown("c_tsrange"), "[\"2026-01-01 00:00:00\",\"2026-12-31 00:00:00\")");
    assert_eq!(get("c_enum"), Value::Text("paid".into()));
    assert_eq!(get("c_domain"), Value::Decimal("42.50".into()));
    assert_eq!(shown("c_composite"), "(\"1 Main St\",Hanoi,100000)");
    assert_eq!(shown("c_xml"), "<a>1</a>");
    assert_eq!(shown("c_tsvector"), "'fosta':1 'studio':2");
    assert_eq!(get("c_oid"), Value::Int(1259));
    assert!(page.rows[1][1..].iter().all(Value::is_null), "row 2 is all NULL");

    // Every value goes back through its display text and parse_input into row 2 (in a transaction that is rolled back by
    // restoring NULLs afterwards), and reads back identical.
    let mut values = Vec::new();
    for (i, col) in page.columns.iter().enumerate().skip(1) {
        let v = &page.rows[0][i];
        let typed = match v {
            Value::Array(_) => parse_input(&col.kind, &serde_json::to_string(&json_of(v)).unwrap()).unwrap(),
            other => parse_input(&col.kind, &other.display().unwrap()).unwrap(),
        };
        values.push((col.name.clone(), typed));
    }
    let zoo = t("public", "type_zoo");
    e.apply_changes(
        &zoo,
        vec![RowChange::Update {
            key: vec![("id".into(), Value::Int(2))],
            values: values.clone(),
        }],
    )
    .await
    .unwrap();
    let again = e.browse(&req).await.unwrap();
    let restore: Vec<(String, Value)> = values.iter().map(|(n, _)| (n.clone(), Value::Null)).collect();
    e.apply_changes(
        &zoo,
        vec![RowChange::Update {
            key: vec![("id".into(), Value::Int(2))],
            values: restore,
        }],
    )
    .await
    .unwrap();
    for (i, col) in again.columns.iter().enumerate().skip(1) {
        assert_eq!(again.rows[1][i], again.rows[0][i], "{}", col.name);
    }
}

fn json_of(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Array(items) => serde_json::Value::Array(items.iter().map(json_of).collect()),
        other => serde_json::Value::String(other.display().unwrap_or_default()),
    }
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_row_changes_in_one_transaction() {
    let e = engine().await;
    let targets = t("sales", "targets");
    let key = |y: i64, r: &str| vec![("year".to_owned(), Value::Int(y)), ("region".to_owned(), Value::Text(r.into()))];
    let s = e
        .apply_changes(
            &targets,
            vec![
                RowChange::Insert {
                    values: vec![
                        ("year".into(), Value::Int(2027)),
                        ("region".into(), Value::Text("AMER".into())),
                        ("amount".into(), Value::Decimal("10.50".into())),
                    ],
                },
                RowChange::Update {
                    key: key(2025, "EMEA"),
                    values: vec![("amount".into(), Value::Decimal("960000".into()))],
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((s.inserted, s.updated), (1, 1));
    let err = e
        .apply_changes(
            &targets,
            vec![
                RowChange::Delete { key: key(2027, "AMER") },
                RowChange::Delete { key: key(1999, "nowhere") },
            ],
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("matched 0 rows"), "{err}");
    let mut req = BrowseRequest::new(targets.clone(), 10);
    req.filter = vec![FilterCond::new("year", FilterOp::Eq, &["2027"])];
    assert_eq!(e.browse(&req).await.unwrap().rows.len(), 1, "the delete was rolled back");
    let bad = e
        .apply_changes(
            &targets,
            vec![RowChange::Insert {
                values: vec![("year".into(), Value::Int(2028))],
            }],
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code.as_deref(), Some("23502"), "{bad}");
    // Clean up and restore.
    e.apply_changes(
        &targets,
        vec![
            RowChange::Delete { key: key(2027, "AMER") },
            RowChange::Update {
                key: key(2025, "EMEA"),
                values: vec![("amount".into(), Value::Decimal("950000".into()))],
            },
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_streaming_limit_cancel_timeout() {
    let e = engine().await;
    let s = e.open_session().await.unwrap();
    let opts = RunOptions {
        read_only: false,
        row_limit: 1000,
        batch_size: 250,
        timeout: None,
    };

    // Several result sets; a billion-row generate_series (in the select list, so it streams instead of materializing) stops at
    // the limit through the cursor, without the server producing the rest.
    let mut sink = CollectSink::default();
    let t0 = std::time::Instant::now();
    s.run(
        "SELECT count(*) FROM orders; SELECT generate_series(1, 1000000000) AS g; SHOW server_version",
        &opts,
        &mut sink,
    )
    .await
    .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert_eq!(sink.sets.len(), 3);
    assert_eq!(sink.sets[0].rows[0][0], Value::Int(4000));
    assert_eq!(sink.sets[1].rows.len(), 1000);
    assert!(sink.sets[1].truncated);
    assert_eq!(sink.batches, 1 + 4 + 1);
    assert_eq!(s.transaction_state(), TxState::Idle);

    // Cancel pg_sleep from another task.
    let cancel = s.canceller();
    let t0 = std::time::Instant::now();
    let c = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel().await.unwrap();
    });
    let err = s.run("SELECT pg_sleep(30)", &opts, &mut CollectSink::default()).await.unwrap_err();
    c.await.unwrap();
    assert_eq!(err.kind, ErrorKind::Cancelled, "{err}");
    assert!(t0.elapsed() < Duration::from_secs(10));

    // Statement timeout.
    let timed = RunOptions {
        read_only: false,
        timeout: Some(Duration::from_millis(300)),
        ..opts.clone()
    };
    let err = s.run("SELECT pg_sleep(5)", &timed, &mut CollectSink::default()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout, "{err}");

    // DML with RETURNING and affected rows.
    let mut sink = CollectSink::default();
    s.run(
        "BEGIN; UPDATE orders SET note = note WHERE id <= 10 RETURNING id; ROLLBACK",
        &opts,
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[1].affected, Some(10));
    assert_eq!(sink.sets[1].rows.len(), 10);
    assert_eq!(PgEngine::classify("UPDATE orders SET note = NULL").kind, StatementKind::Destructive);
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_transaction_state() {
    let e = engine().await;
    let s = e.open_session().await.unwrap();
    let opts = RunOptions::default();
    s.run("BEGIN", &opts, &mut CollectSink::default()).await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Active);
    // A query inside the user's transaction uses the cursor without ending the transaction.
    let mut sink = CollectSink::default();
    s.run(
        "UPDATE sales.targets SET amount = amount + 1 WHERE year = 2026; SELECT amount FROM sales.targets WHERE year = 2026",
        &opts,
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[1].rows[0][0], Value::Decimal("1500001.00".into()));
    assert_eq!(s.transaction_state(), TxState::Active);
    assert!(s.run("SELECT 1/0", &opts, &mut CollectSink::default()).await.is_err());
    assert_eq!(s.transaction_state(), TxState::Failed);
    s.rollback().await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Idle);
    let mut sink = CollectSink::default();
    s.run("SELECT amount FROM sales.targets WHERE year = 2026", &opts, &mut sink)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Decimal("1500000.00".into()));
    s.begin().await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Active);
    s.commit().await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Idle);
}

#[tokio::test]
#[ignore]
async fn live_engine_postgres_read_only_backstop_is_reasserted() {
    let e = engine().await;
    let s = e.open_session().await.unwrap();
    let ro = RunOptions {
        read_only: true,
        ..RunOptions::default()
    };
    let rw = RunOptions::default();
    let probe = "CREATE TEMP TABLE fosta_ro_probe (a int)";
    let refused = |err: crate::engines::common::EngineError| assert_eq!(err.code.as_deref(), Some("25006"), "{err}");

    // A statement that switched the session default off (classified Admin; here run past the gateway) does not survive
    // into the next read-only run, nor does RESET ALL or DISCARD ALL.
    for lift in [
        "SET default_transaction_read_only = off",
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        "RESET ALL",
        "DISCARD ALL",
    ] {
        s.run(lift, &ro, &mut CollectSink::default()).await.unwrap();
        refused(s.run(probe, &ro, &mut CollectSink::default()).await.unwrap_err());
    }

    // A read-write transaction left open refuses read-only runs until it ends.
    s.run("BEGIN READ WRITE", &ro, &mut CollectSink::default()).await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Active);
    for sql in [probe, "SELECT 1"] {
        let err = s.run(sql, &ro, &mut CollectSink::default()).await.unwrap_err();
        assert!(err.message.contains("read-write transaction is open"), "{err}");
    }
    s.rollback().await.unwrap();
    s.run("SELECT 1", &ro, &mut CollectSink::default()).await.unwrap();

    // In a failed transaction (where the SET cannot run) a read-only run may only roll back.
    assert!(s.run("BEGIN; SELECT 1/0", &rw, &mut CollectSink::default()).await.is_err());
    assert_eq!(s.transaction_state(), TxState::Failed);
    let err = s.run("ROLLBACK; SELECT 1", &ro, &mut CollectSink::default()).await.unwrap_err();
    assert!(err.message.contains("Roll it back on its own"), "{err}");
    s.run("ROLLBACK", &ro, &mut CollectSink::default()).await.unwrap();
    assert_eq!(s.transaction_state(), TxState::Idle);

    // A read-only transaction is fine, and read-write runs still work afterwards.
    s.run("BEGIN", &ro, &mut CollectSink::default()).await.unwrap();
    s.run("SELECT 1", &ro, &mut CollectSink::default()).await.unwrap();
    s.rollback().await.unwrap();
    s.run(probe, &rw, &mut CollectSink::default()).await.unwrap();
    s.run("DROP TABLE fosta_ro_probe", &rw, &mut CollectSink::default()).await.unwrap();
}
