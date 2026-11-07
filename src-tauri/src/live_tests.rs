//! Driver tests against the containers in `dev/docker-compose.yml`.
//! Ignored by default; run with:
//!   cargo test live_ -- --ignored --test-threads=1

use std::collections::HashMap;

use crate::{
    config::ConnectionProfile,
    drivers::{
        self,
        types::{BrowseRequest, CellValue, RowChange},
        Driver,
    },
};

fn profile(id: &str, driver: &str, port: u16, user: &str, database: &str, options: &[(&str, &str)]) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: id.into(),
        driver: driver.into(),
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: Some(database.into()),
        save_password: true,
        options: options
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>(),
    }
}

fn browse(database: &str, table: &str, filter: Option<&str>) -> BrowseRequest {
    BrowseRequest {
        connection_id: String::new(),
        database: database.into(),
        table: table.into(),
        offset: 0,
        limit: 5,
        order_by: None,
        descending: false,
        filter: filter.map(str::to_owned),
    }
}

fn cell(column: &str, value: &str) -> CellValue {
    CellValue {
        column: column.into(),
        value: Some(value.into()),
    }
}

/// Creates a scratch database, checks it is listed, then drops it with `drop_sql`.
async fn create_and_drop(driver: &dyn Driver, name: &str, collection: Option<&str>, run_in: Option<&str>, drop_sql: &str) {
    driver.create_database(name, collection).await.expect("create_database");
    assert!(
        driver.list_databases().await.expect("list").iter().any(|item| item == name),
        "{name} not listed"
    );
    driver.open_session("drop").await.expect("session");
    driver
        .run_query("drop", run_in.map(str::to_owned), drop_sql.into())
        .await
        .expect("drop");
    driver.close_session("drop");
}

async fn exercise(driver: &dyn Driver, database: &str, table: &str, filter: &str, query: &str) {
    let databases = driver.list_databases().await.expect("list_databases");
    assert!(
        databases.iter().any(|name| name == database),
        "{database} missing from {databases:?}"
    );
    let tables = driver.list_tables(database).await.expect("list_tables");
    assert!(tables.iter().any(|item| item.name == table), "{table} missing");
    let design = driver.describe_table(database, table).await.expect("describe_table");
    assert!(!design.columns.is_empty());
    let page = driver.browse_table(&browse(database, table, None)).await.expect("browse_table");
    assert_eq!(page.rows.len(), 5);
    let filtered = driver
        .browse_table(&browse(database, table, Some(filter)))
        .await
        .expect("filtered browse");
    assert!(
        filtered.total < page.total,
        "filter did not narrow: {} vs {}",
        filtered.total,
        page.total
    );
    driver.open_session("live").await.expect("open_session");
    let outcome = driver
        .run_query("live", Some(database.into()), query.into())
        .await
        .expect("run_query");
    assert!(outcome.sets.iter().any(|set| !set.rows.is_empty()), "query returned nothing");
    driver.close_session("live");
    println!(
        "{table}: {} tables, {} columns, total {}, filtered {}, query sets {}",
        tables.len(),
        design.columns.len(),
        page.total,
        filtered.total,
        outcome.sets.len()
    );
}

#[tokio::test]
#[ignore]
async fn live_saved_passwords_are_readable() {
    for id in ["local-mariadb", "local-mssql", "local-mongo"] {
        assert!(
            crate::secrets::load_password(id).expect("keyring").is_some(),
            "no saved password for {id}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn live_mariadb() {
    let profile = profile("live-mariadb", "mariadb", 3306, "root", "shop", &[]);
    let (driver, version) = drivers::open(&profile, "studio-root").await.expect("connect");
    println!("MariaDB {version}");
    exercise(
        driver.as_ref(),
        "shop",
        "products",
        "stock < 5",
        "SELECT category_id, COUNT(*) FROM products GROUP BY category_id",
    )
    .await;
    create_and_drop(driver.as_ref(), "studio_scratch", None, None, "DROP DATABASE `studio_scratch`").await;
    let relations = driver.table_relations("shop", "products").await.expect("relations");
    assert!(!relations.outgoing.is_empty() && !relations.incoming.is_empty());
    // MySQL counts changed rows, so write a different value and then restore it.
    for name in ["Outdoor (edited)", "Outdoor"] {
        let changed = driver
            .apply_changes(
                "shop",
                "categories",
                vec![RowChange::Update {
                    key: vec![cell("id", "8")],
                    values: vec![cell("name", name)],
                }],
            )
            .await
            .expect("apply_changes");
        assert_eq!(changed, 1);
    }
    driver.close().await;
}

#[tokio::test]
#[ignore]
async fn live_mssql() {
    let profile = profile(
        "live-mssql",
        "mssql",
        1433,
        "sa",
        "ERP",
        &[("encrypt", "true"), ("trustServerCertificate", "true")],
    );
    let (driver, version) = drivers::open(&profile, "Studio#Pass2026").await.expect("connect");
    println!("SQL Server {version}");
    exercise(
        driver.as_ref(),
        "ERP",
        "sales.Invoices",
        "Status = 'paid'",
        "SELECT TOP 5 * FROM sales.vMonthlyRevenue ORDER BY Month",
    )
    .await;
    create_and_drop(driver.as_ref(), "StudioScratch", None, None, "DROP DATABASE [StudioScratch]").await;
    let relations = driver.table_relations("ERP", "sales.Invoices").await.expect("relations");
    assert!(!relations.outgoing.is_empty());
    let changed = driver
        .apply_changes(
            "ERP",
            "Warehouses",
            vec![RowChange::Update {
                key: vec![cell("WarehouseId", "1")],
                values: vec![cell("City", "Soc Trang")],
            }],
        )
        .await
        .expect("apply_changes");
    assert_eq!(changed, 1);
    driver.close().await;
}

/// The connection-string tab stores the URI without its password; the driver adds it back.
#[tokio::test]
#[ignore]
async fn live_mongo_uri_without_password() {
    let profile = profile(
        "live-mongo-uri",
        "mongodb",
        27017,
        "root",
        "admin",
        &[("uri", "mongodb://root@127.0.0.1:27017/?authSource=admin")],
    );
    let (driver, _) = drivers::open(&profile, "studio-root").await.expect("connect with uri");
    assert!(driver.list_databases().await.expect("list").iter().any(|name| name == "events"));
    driver.close().await;
}

#[tokio::test]
#[ignore]
async fn live_mongo() {
    let profile = profile("live-mongo", "mongodb", 27017, "root", "admin", &[]);
    let (driver, version) = drivers::open(&profile, "studio-root").await.expect("connect");
    println!("MongoDB {version}");
    exercise(
        driver.as_ref(),
        "events",
        "clicks",
        r#"{ "type": "purchase" }"#,
        r#"{ "aggregate": "clicks", "pipeline": [ { "$group": { "_id": "$device", "n": { "$sum": 1 } } } ], "cursor": {} }"#,
    )
    .await;
    create_and_drop(
        driver.as_ref(),
        "studio_scratch",
        Some("first"),
        Some("studio_scratch"),
        r#"{ "dropDatabase": 1 }"#,
    )
    .await;
    // Mongo also counts modified documents; flip the value and restore it.
    for plan in ["pro", "free"] {
        let changed = driver
            .apply_changes(
                "events",
                "users",
                vec![RowChange::Update {
                    key: vec![cell("_id", "1")],
                    values: vec![cell("plan", plan)],
                }],
            )
            .await
            .expect("apply_changes");
        assert!(changed <= 1);
    }
    driver.close().await;
}

/// The ER diagram's data on each engine: tables, columns and the expected foreign keys.
#[tokio::test]
#[ignore]
async fn live_schema_graphs() {
    let cases = [
        (profile("g-maria", "mariadb", 3306, "root", "shop", &[]), "studio-root", "shop", 4),
        (
            profile(
                "g-mssql",
                "mssql",
                1433,
                "sa",
                "ERP",
                &[("encrypt", "true"), ("trustServerCertificate", "true")],
            ),
            "Studio#Pass2026",
            "ERP",
            3,
        ),
        (
            profile("g-mongo", "mongodb", 27017, "root", "admin", &[]),
            "studio-root",
            "events",
            0,
        ),
    ];
    for (profile, password, database, expected_links) in cases {
        let (driver, _) = drivers::open(&profile, password).await.expect("connect");
        let started = std::time::Instant::now();
        let slow = crate::schema::schema_graph_per_table(driver.as_ref(), database)
            .await
            .expect("per-table schema_graph");
        let per_table = started.elapsed();
        let started = std::time::Instant::now();
        let _warm = crate::schema::schema_graph(driver.as_ref(), database).await.expect("schema_graph");
        let first = started.elapsed();
        let started = std::time::Instant::now();
        let graph = crate::schema::schema_graph(driver.as_ref(), database).await.expect("schema_graph");
        let bulk = started.elapsed();
        println!("{database}: per-table {per_table:?}, bulk {bulk:?} (cold {first:?})");
        assert_eq!(
            serde_json::to_string(&graph).unwrap(),
            serde_json::to_string(&slow).unwrap(),
            "{database} bulk graph differs"
        );
        let columns: usize = graph.tables.iter().map(|table| table.columns.len()).sum();
        let keys: usize = graph
            .tables
            .iter()
            .flat_map(|table| &table.columns)
            .filter(|column| column.primary)
            .count();
        println!(
            "{database}: {} tables, {columns} columns, {keys} key columns, {} relations",
            graph.tables.len(),
            graph.relations.len()
        );
        for relation in &graph.relations {
            println!(
                "  {}.{:?} -> {}.{:?}",
                relation.table, relation.columns, relation.referenced_table, relation.referenced_columns
            );
        }
        assert_eq!(graph.relations.len(), expected_links, "{database} relations");
        assert!(keys > 0);
        driver.close().await;
    }
}

/// The assistant end to end against Fosta: needs FOSTA_KEY in the environment.
#[tokio::test]
#[ignore]
async fn live_assistant_tools() {
    use std::sync::{Arc, Mutex};
    let Ok(key) = std::env::var("FOSTA_KEY") else {
        println!("FOSTA_KEY not set; skipped");
        return;
    };
    let manager = crate::drivers::DbManager::default();
    manager
        .connect(&profile("chat-maria", "mariadb", 3306, "root", "shop", &[]), "studio-root")
        .await
        .expect("connect");
    let settings = crate::ai::AiSettings {
        provider: crate::ai::AiProvider::Fosta,
        base_url: String::new(),
        model: "cx/gpt-6-astra".into(),
    };
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    let channel = tauri::ipc::Channel::new(move |body| {
        if let tauri::ipc::InvokeResponseBody::Json(json) = body {
            sink.lock().unwrap().push(json);
        }
        Ok(())
    });
    let request: crate::ai::ChatRequest = serde_json::from_value(serde_json::json!({
        "id": "t1",
        "messages": [{ "role": "user", "content": "Open the ER diagram of shop centred on orders, then write a query listing each customer's total paid order amount." }],
        "context": { "connectionId": "chat-maria", "connectionName": "Local MariaDB", "driverLabel": "MariaDB", "database": "shop" }
    }))
    .unwrap();
    let reply = crate::ai::chat(&settings, Some(key), &manager, &crate::ai::AiRuns::default(), request, channel)
        .await
        .expect("chat");
    let events = seen.lock().unwrap().clone();
    for event in events.iter().filter(|event| !event.contains("\"text\"")) {
        println!("event {event}");
    }
    println!("reply:\n{reply}");
    assert!(events.iter().any(|event| event.contains("openDiagram")), "no diagram action");
    assert!(reply.contains("```"), "no code block");
}

/// Writes into the real Downloads folder (twice, to check the " (2)" suffix), then cleans up.
#[test]
#[ignore]
fn live_save_download() {
    let first = crate::files::save_download("db-studio-selftest.csv", b"a,b\n1,2\n").expect("save");
    let second = crate::files::save_download("db-studio-selftest.csv", b"x").expect("save again");
    println!("{first}\n{second}");
    assert!(second.ends_with("db-studio-selftest (2).csv"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a,b\n1,2\n");
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();
}
