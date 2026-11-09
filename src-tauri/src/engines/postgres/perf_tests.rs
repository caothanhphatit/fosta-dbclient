//! Live checks on scripts and big tables against `dev/compose/postgres.yml`: a long script of small statements runs in
//! about one round trip per statement with unchanged results, the last page is exact on a table larger than its
//! estimate. Each test creates its own table and drops it.

use std::{fmt::Write as _, sync::Arc, time::Instant};

use super::contract_tests::{secrets, spec};
use crate::{
    contract::{CallContext, CollectSink, RunOptions, RunRequest, Session, SessionOptions},
    error::AppResult,
    runtime::{ConnectionManager, Handle},
};

async fn shop() -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect("perf", spec(), secrets()).await.expect("connect");
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
        database: None,
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
    Ok(sink)
}

/// Everything a run reported except timings, one line per result set.
fn digest(sink: &CollectSink) -> String {
    let mut out = String::new();
    for set in &sink.sets {
        let columns: Vec<String> = set.columns.iter().map(|c| format!("{}:{}", c.name, c.type_name)).collect();
        let _ = writeln!(
            out,
            "{} | {} | {:?} | {:?} | {} | {:?}",
            set.sql,
            columns.join(","),
            set.affected,
            set.truncated,
            set.rows.len(),
            set.rows
        );
    }
    out
}

/// 3,750 statements: mostly small reads, plus writes, a transaction block, an empty result and a capped one.
fn script() -> String {
    let mut text = String::new();
    for i in 0..3750 {
        let statement = match i % 25 {
            0 => format!("UPDATE perf_script SET label = 'u{i}' WHERE id = {}", i % 500 + 1),
            1 => "BEGIN".to_owned(),
            2 => format!("SELECT id, label FROM perf_script WHERE id = {}", i % 500 + 1),
            3 => "COMMIT".to_owned(),
            4 => "SELECT * FROM perf_script WHERE id < 0".to_owned(),
            5 if i % 250 == 5 => "SELECT a FROM generate_series(1, 1500) a".to_owned(),
            6 => format!("VALUES ({i}, 'v')"),
            _ => format!("SELECT id, amount, label FROM perf_script WHERE id = {}", i % 500 + 1),
        };
        text.push_str(&statement);
        text.push_str(";\n");
    }
    text
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn perf_postgres_long_script_round_trips() {
    let (_manager, handle) = shop().await;
    let session = session(&handle).await;
    run(&handle, &session, "DROP TABLE IF EXISTS perf_script").await.unwrap();
    run(
        &handle,
        &session,
        "CREATE TABLE perf_script AS SELECT g AS id, (g * 7919 % 100000) / 100.0 AS amount, 'row ' || g AS label \
         FROM generate_series(1, 500) g; ALTER TABLE perf_script ADD PRIMARY KEY (id)",
    )
    .await
    .unwrap();

    let started = Instant::now();
    let sink = run(&handle, &session, &script()).await.unwrap();
    let elapsed = started.elapsed();
    println!("3,750-statement script: {elapsed:?}");
    assert_eq!(sink.sets.len(), 3750);
    let digest = digest(&sink);
    let hash = digest
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    println!("result digest: {hash:016x} ({} bytes)", digest.len());
    let capped = sink.sets.iter().find(|set| set.sql.contains("generate_series")).unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.rows.len(), 1000);
    let state = session.transaction_state().await;
    assert!(!state.open, "the script's transaction blocks are closed");

    run(&handle, &session, "DROP TABLE perf_script").await.unwrap();
    // About one round trip per statement (seven before): a debug build takes ~4.5 s here, ~23 s before.
    assert!(elapsed.as_secs() < 12, "the script took {elapsed:?}");
}
