//! Live checks on big tables against `dev/compose/mariadb.yml`: query tabs stop reading at the row cap, cancelled pages
//! free their pooled connection, the last page is exact on a table larger than its estimate. Each test creates its own
//! table in `shop` and drops it.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::contract_tests::{secrets, spec};
use crate::{
    contract::{
        BrowseRequest, CallContext, CollectSink, Filter, NodeKind, ObjectRef, RawFilterLang, RunOptions, RunRequest, Session,
        SessionOptions, CANCELLED,
    },
    error::AppResult,
    gateway::{
        self,
        rules::{Preset, SafetySettings},
        Context,
    },
    runtime::{ConnectionManager, Handle},
};

fn open_workspace() -> Context {
    Context {
        workspace_id: "perf".into(),
        workspace_name: "Perf".into(),
        connection_id: "perf".into(),
        connection_name: "perf".into(),
        safety: SafetySettings::preset(Preset::Open),
    }
}

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

/// A page whose read takes long on the server: its filter counts a cross join of a billion rows.
fn slow_page(object: &ObjectRef) -> BrowseRequest {
    let mut request = BrowseRequest::new(object.clone(), 50);
    request.filter = Filter::Raw {
        language: RawFilterLang::Sql,
        text: "`id` = 1 AND (SELECT COUNT(*) FROM perf_cancel a, perf_cancel b, perf_cancel c) > 0".into(),
    };
    request
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn perf_mariadb_cancelled_pages_free_the_pool() {
    let (manager, handle) = shop().await;
    let manager = Arc::new(manager);
    let session = session(&handle).await;
    let object = big_table(&handle, &session, "perf_cancel", 1000).await;
    let context = open_workspace();

    // Four slow pages take every pooled connection: a fifth request says so instead of blaming the network.
    let mut slow = Vec::new();
    for i in 0..4 {
        let (manager, context, request) = (Arc::clone(&manager), context.clone(), slow_page(&object));
        slow.push(tokio::spawn(async move {
            let id = format!("slow-{i}");
            let started = Instant::now();
            let outcome = gateway::browse_cancellable(&manager, &context, "perf", &request, Some(&id)).await;
            (outcome, started.elapsed())
        }));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let error = gateway::browse(&manager, &context, "perf", &BrowseRequest::new(object.clone(), 50))
        .await
        .expect_err("the pool is busy");
    assert!(error.to_string().contains("All 4 connections"), "{error}");

    // Cancelling stops the server's work and frees the connections at once.
    let cancelled_at = Instant::now();
    for i in 0..4 {
        manager.requests.cancel(&format!("slow-{i}"));
    }
    for task in slow {
        let (outcome, _) = task.await.unwrap();
        assert_eq!(outcome.expect_err("cancelled").code(), Some(CANCELLED));
    }
    let stop = cancelled_at.elapsed();
    let started = Instant::now();
    let page = gateway::browse(&manager, &context, "perf", &BrowseRequest::new(object.clone(), 50))
        .await
        .unwrap();
    let next = started.elapsed();
    println!("4 cancelled pages stopped in {stop:?}; the next page took {next:?}");
    assert_eq!(page.rows.len(), 50);
    assert!(
        stop < Duration::from_secs(3) && next < Duration::from_secs(2),
        "{stop:?} / {next:?}"
    );
    let sink = run(
        &handle,
        &session,
        "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE '%perf_cancel c%' AND INFO NOT LIKE '%PROCESSLIST%'",
    )
    .await
    .unwrap();
    assert_eq!(
        sink.sets[0].rows[0][0],
        crate::contract::Value::Int(0),
        "no read is left running on the server"
    );

    // A cancel that arrives before its request still cancels it.
    manager.requests.cancel("early");
    let error = gateway::browse_cancellable(&manager, &context, "perf", &slow_page(&object), Some("early"))
        .await
        .expect_err("cancelled before it started");
    assert_eq!(error.code(), Some(CANCELLED));

    run(&handle, &session, "DROP TABLE perf_cancel").await.unwrap();
}

fn ids(page: &crate::contract::Page) -> Vec<i64> {
    page.rows
        .iter()
        .map(|row| match &row[0] {
            crate::contract::Value::Int(id) => *id,
            other => panic!("id {other:?}"),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn perf_mariadb_last_page_reads_from_the_end() {
    let (manager, handle) = shop().await;
    let session = session(&handle).await;
    let object = big_table(&handle, &session, "perf_last", 1_000_000).await;
    run(&handle, &session, "ANALYZE TABLE perf_last").await.unwrap();
    let context = open_workspace();

    // Unsorted: the last 200 rows by key, in order, with the exact total (the statistics only estimate it).
    let mut request = BrowseRequest::new(object.clone(), 200);
    request.reverse = true;
    request.exact_total = true;
    let started = Instant::now();
    let last = gateway::browse(&manager, &context, "perf", &request).await.unwrap();
    println!("last page by key: {:?}", started.elapsed());
    assert_eq!(ids(&last), (999_801..=1_000_000).collect::<Vec<_>>());
    assert_eq!(last.total.map(|total| (total.value, total.exact)), Some((1_000_000, true)));
    assert!(last.has_more);
    // The page before it, by keyset from its first row.
    request.after = last.next_keyset.clone();
    request.exact_total = false;
    let before = gateway::browse(&manager, &context, "perf", &request).await.unwrap();
    assert_eq!(ids(&before), (999_601..=999_800).collect::<Vec<_>>());

    // Sorted by an unindexed column: the same rows a deep OFFSET finds, without walking the table.
    let mut sorted = BrowseRequest::new(object.clone(), 200);
    sorted.sort = vec![crate::contract::SortKey::asc("amount")];
    sorted.offset = 999_800;
    let started = Instant::now();
    let by_offset = gateway::browse(&manager, &context, "perf", &sorted).await.unwrap();
    let offset_took = started.elapsed();
    sorted.offset = 0;
    sorted.reverse = true;
    let started = Instant::now();
    let by_end = gateway::browse(&manager, &context, "perf", &sorted).await.unwrap();
    println!("sorted last page: OFFSET {offset_took:?}, from the end {:?}", started.elapsed());
    assert_eq!(ids(&by_end), ids(&by_offset));
    assert_eq!(by_end.rows.len(), 200);

    run(&handle, &session, "DROP TABLE perf_last").await.unwrap();
}
