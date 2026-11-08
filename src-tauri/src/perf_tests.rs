//! Rough timings and payload sizes against the dev MariaDB container, to compare the IPC paths across refactors.
//! Ignored by default; run with:
//!   cargo test perf_ -- --ignored --nocapture --test-threads=1

use std::{collections::HashMap, time::Instant};

use crate::{
    config::ConnectionProfile,
    drivers::{self, types::BrowseRequest, Limits},
};

fn mariadb() -> ConnectionProfile {
    ConnectionProfile {
        id: "perf-mariadb".into(),
        name: "perf".into(),
        driver: "mariadb".into(),
        host: "127.0.0.1".into(),
        port: 3306,
        user: "root".into(),
        database: Some("shop".into()),
        save_password: false,
        options: HashMap::new(),
        safety: None,
    }
}

const QUERY_10K: &str = "SELECT o.id, o.customer_id, o.status, o.ordered_at, c.full_name, c.email, c.city \
     FROM orders o JOIN customers c ON c.id = o.customer_id ORDER BY o.id LIMIT 10000";

#[tokio::test]
#[ignore]
async fn perf_open_table_and_10k_query() {
    let (driver, _) = drivers::open(&mariadb(), "studio-root").await.expect("connect");
    // Open a table: first page of 200 rows with the total, as the table view does.
    let mut best = u128::MAX;
    let mut page_bytes = 0;
    for _ in 0..5 {
        let started = Instant::now();
        let page = driver
            .browse_table(
                &BrowseRequest {
                    connection_id: String::new(),
                    database: "shop".into(),
                    table: "orders".into(),
                    offset: 0,
                    limit: 200,
                    order_by: None,
                    descending: false,
                    filter: None,
                },
                &Limits::default(),
            )
            .await
            .expect("browse");
        page_bytes = serde_json::to_vec(&page).unwrap().len();
        best = best.min(started.elapsed().as_micros());
    }
    println!("PERF open table orders (200 rows): best {:.1} ms, payload {page_bytes} bytes", best as f64 / 1000.0);

    driver.open_session("perf").await.expect("session");
    let limits = Limits {
        max_rows: 10_000,
        ..Limits::default()
    };
    let started = Instant::now();
    let outcome = driver
        .run_query("perf", Some("shop".into()), QUERY_10K.into(), &limits)
        .await
        .expect("query");
    let elapsed = started.elapsed().as_micros();
    let payload = serde_json::to_vec(&outcome).unwrap().len();
    let rows: usize = outcome.sets.iter().map(|set| set.rows.len()).sum();
    println!(
        "PERF query 10k rows x 7 columns: {rows} rows in {:.1} ms, payload {payload} bytes",
        elapsed as f64 / 1000.0
    );
    driver.close_session("perf");
    driver.close().await;
}
