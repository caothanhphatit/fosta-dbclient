//! Live checks on big tables against `dev/compose/mariadb.yml`: query tabs stop reading at the row cap, cancelled pages
//! free their pooled connection, the last page is exact on a table larger than its estimate. Each test creates its own
//! table in `shop` and drops it.

use std::{sync::Arc, time::Instant};

use super::contract_tests::{secrets, spec};
use crate::{
    contract::{CallContext, CollectSink, NodeKind, ObjectRef, RunOptions, RunRequest, Session, SessionOptions},
    error::AppResult,
    runtime::{ConnectionManager, Handle},
};

async fn shop() -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect("perf", spec(Some("shop")), secrets()).await.expect("connect");
    let handle = manager.handle("perf").unwrap();
    (manager, handle)
}

async fn session(handle: &Handle) -> Arc<dyn Session> {
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: Some("shop".into()),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
    Ok(sink)
}

/// `shop.<name>` with `rows` rows: `id` (primary key), `amount`, `label`.
async fn big_table(handle: &Handle, session: &Arc<dyn Session>, name: &str, rows: u32) -> ObjectRef {
    run(handle, session, &format!("DROP TABLE IF EXISTS {name}")).await.unwrap();
    run(
        handle,
        session,
        &format!("CREATE TABLE {name} (id INT PRIMARY KEY, amount DECIMAL(10,2) NOT NULL, label VARCHAR(40))"),
    )
    .await
    .unwrap();
    run(
        handle,
        session,
        &format!("INSERT INTO {name} SELECT seq, (seq * 7919 % 100000) / 100.0, CONCAT('row ', seq) FROM seq_1_to_{rows}"),
    )
    .await
    .unwrap();
    ObjectRef::new(Some("shop"), None, name, NodeKind::TABLE)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn perf_mariadb_truncated_query_stops_reading_at_the_cap() {
    let (_manager, handle) = shop().await;
    let session = session(&handle).await;
    big_table(&handle, &session, "perf_truncate", 1_000_000).await;

    let started = Instant::now();
    let sink = run(&handle, &session, "SELECT * FROM perf_truncate").await.unwrap();
    let elapsed = started.elapsed();
    println!("SELECT * on 1M rows, cap 1000: {elapsed:?}");
    assert_eq!(sink.sets.len(), 1);
    assert_eq!(sink.sets[0].rows.len(), 1000);
    assert!(sink.sets[0].truncated);

    // The session stays usable, and its next statement is not hit by the stop.
    let sink = run(&handle, &session, "SELECT 1").await.unwrap();
    assert_eq!(sink.sets[0].rows.len(), 1);
    // A result that is already complete when the stop arrives: the stop must not hit the next run.
    for _ in 0..5 {
        let sink = run(&handle, &session, "SELECT * FROM perf_truncate LIMIT 1001").await.unwrap();
        assert!(sink.sets[0].truncated);
        let sink = run(&handle, &session, "SELECT COUNT(*) FROM perf_truncate").await.unwrap();
        assert_eq!(sink.sets[0].rows.len(), 1);
    }

    // A capped statement that is not the last one is read to its end, so the script goes on.
    let sink = run(&handle, &session, "SELECT * FROM perf_truncate; SELECT 2").await.unwrap();
    assert_eq!(sink.sets.len(), 2);
    assert!(sink.sets[0].truncated);
    assert_eq!(sink.sets[1].rows.len(), 1);

    run(&handle, &session, "DROP TABLE perf_truncate").await.unwrap();
    assert!(elapsed.as_millis() < 1500, "the capped query took {elapsed:?}");
}
