//! Rough timings and payload sizes against the dev MariaDB container, to compare the IPC paths across refactors.
//! Ignored by default; run with:
//!   cargo test perf_ -- --ignored --nocapture --test-threads=1

use std::time::Instant;

use crate::{
    config::ConnectionProfile,
    contract::{BrowseRequest, CallContext, CollectSink, ConnectSecrets, ObjectRef, RunOptions, RunRequest, SessionOptions},
    runtime::ConnectionManager,
};

fn mariadb() -> ConnectionProfile {
    ConnectionProfile {
        id: "perf-mariadb".into(),
        name: "perf".into(),
        engine: "mariadb".into(),
        host: "127.0.0.1".into(),
        port: 3306,
        user: "root".into(),
        database: Some("shop".into()),
        ..Default::default()
    }
}

const QUERY_10K: &str = "SELECT o.id, o.customer_id, o.status, o.ordered_at, c.full_name, c.email, c.city \
     FROM orders o JOIN customers c ON c.id = o.customer_id ORDER BY o.id LIMIT 10000";

/// Open a table (a typed page of 200 rows through `TabularData::browse`) and a 10k-row query streamed through a session
/// into a `CollectSink`, on the native MariaDB adapter.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn perf_native_open_table_and_10k_query() {
    let manager = ConnectionManager::default();
    manager
        .connect("perf", mariadb().spec(), ConnectSecrets::password("studio-root"))
        .await
        .expect("connect");
    let handle = manager.handle("perf").unwrap();
    let mut best = u128::MAX;
    let mut page_bytes = 0;
    for _ in 0..5 {
        let mut request = BrowseRequest::new(ObjectRef::table("shop", None, "orders"), 200);
        request.estimate_total = true;
        let started = Instant::now();
        let page = handle
            .call("browsed", |conn| async move {
                conn.tabular().unwrap().browse(&request, &CallContext::default()).await
            })
            .await
            .expect("browse");
        page_bytes = serde_json::to_vec(&page).unwrap().len();
        best = best.min(started.elapsed().as_micros());
    }
    println!(
        "PERF native open table orders (200 rows): best {:.1} ms, payload {page_bytes} bytes",
        best as f64 / 1000.0
    );

    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session;
    let options = RunOptions {
        row_limit: 10_000,
        ..RunOptions::default()
    };
    let request = RunRequest {
        text: QUERY_10K.into(),
        database: Some("shop".into()),
        offset: 0,
    };
    let mut sink = CollectSink::default();
    let started = Instant::now();
    handle.run(session.clone(), request, options, &mut sink).await.expect("query");
    let elapsed = started.elapsed().as_micros();
    let payload: usize = sink
        .sets
        .iter()
        .map(|set| serde_json::to_vec(&set.columns).unwrap().len() + serde_json::to_vec(&set.rows).unwrap().len())
        .sum();
    let rows: usize = sink.sets.iter().map(|set| set.rows.len()).sum();
    println!(
        "PERF native query 10k rows x 7 columns: {rows} rows in {:.1} ms ({} batches), payload {payload} bytes",
        elapsed as f64 / 1000.0,
        sink.batches
    );
    session.close().await;
    manager.disconnect("perf").await;
}
