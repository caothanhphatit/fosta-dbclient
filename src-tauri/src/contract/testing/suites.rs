//! Contract suites: the behaviour every engine must show, per capability, against a live server. Each engine's
//! `contract_tests.rs` builds a [`Fixture`] and calls [`contract_tests!`](crate::contract_tests), which generates one
//! test per suite (`contract_<engine>_<suite>`, ignored unless its container runs) plus the pure classification test.
//!
//! Every suite connects through the `ConnectionManager` (registry, crash containment), seeds the fixture's data first,
//! and checks the advertised capabilities: an operation the connection does not advertise must fail with
//! `UNSUPPORTED_CAPABILITY`. Known gaps of an adapter are listed in the fixture and skipped with a printed note, so
//! the list is the adapter's to-do list.

use std::{sync::Arc, time::Duration};

use crate::{
    contract::{
        errors::CONFLICT, ApplyResult, BrowseRequest, CallContext, Capability, Category, ClassifyInput, CollectSink, ConnectSecrets,
        ConnectionSpec, Field, Filter, FilterCond, FilterOp, NodeKind, ObjectRef, RowChange, RowChangeSet, RunOptions, RunRequest, Session,
        SessionOptions, SortKey, TransactionOp, TreeRequest, Value,
    },
    error::{AppError, AppResult},
    runtime::{registry, ConnectionManager, Handle},
};

/// What an adapter cannot do yet; the suites skip those checks with a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// Structured `Filter::Conditions`.
    StructuredFilter,
    /// Updates and deletes are not guarded by their original values or row version.
    Concurrency,
    /// `Page::next_keyset`.
    Keyset,
    /// Values come back as text instead of their typed variants.
    TypedValues,
    /// Driver errors are not mapped to vendor codes and positions.
    ErrorMapping,
    /// Foreign keys pointing at a table are not listed.
    ReferencedBy,
}

pub struct Fixture {
    pub engine: &'static str,
    pub spec: ConnectionSpec,
    pub secrets: ConnectSecrets,
    /// The seeded table (`contract_items`, 1,000 rows, keys 1..=1000) and its child table (200 rows referencing it).
    pub table: ObjectRef,
    pub child: ObjectRef,
    pub key_column: &'static str,
    /// The text column the edit suites change (unique, `item 0001`…).
    pub name_column: &'static str,
    /// Chunks run in a session before each suite to (re)create the data, in `seed_database`.
    pub seed: Vec<String>,
    pub seed_database: Option<&'static str>,
    /// A raw filter selecting one row by key (`id = 5`, `{ "_id": 5 }`).
    pub key_filter: fn(i64) -> Filter,
    /// A statement that runs for a long time, for the cancel suite.
    pub sleep_statement: Option<&'static str>,
    /// A statement with a syntax error and one that violates the unique name.
    pub syntax_error: &'static str,
    pub constraint_violation: &'static str,
    /// Two statements in one script, the second returning rows.
    pub multi_statement: &'static str,
    /// An insert of key 5000 (for transaction checks) and a count of key 5000 rows.
    pub insert_probe: &'static str,
    pub count_probe: &'static str,
    pub classify_samples: &'static [(&'static str, Category)],
    pub language: &'static str,
    pub gaps: &'static [Gap],
}

impl Fixture {
    fn has_gap(&self, gap: Gap) -> bool {
        let skipped = self.gaps.contains(&gap);
        if skipped {
            println!("{}: skipped (known gap {gap:?})", self.engine);
        }
        skipped
    }
}

pub const CONNECTION: &str = "contract";

/// Connects, then seeds the data.
pub async fn connect(f: &Fixture) -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    let info = manager
        .connect(CONNECTION, f.spec.clone(), f.secrets.clone())
        .await
        .unwrap_or_else(|error| panic!("{}: connect failed: {error}", f.engine));
    assert_eq!(info.engine, f.engine);
    let handle = manager.handle(CONNECTION).unwrap();
    if !f.seed.is_empty() {
        let session = open_session(&handle).await;
        for chunk in &f.seed {
            run_text(&handle, &session, chunk, f.seed_database)
                .await
                .unwrap_or_else(|error| panic!("{}: seeding failed: {error}\n{chunk}", f.engine));
        }
        session.close().await;
    }
    (manager, handle)
}

async fn open_session(handle: &Handle) -> Arc<dyn Session> {
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("open session")
        .session
}

/// Runs text in a session the way the gateway does (contained, engine id on errors).
async fn run_text(handle: &Handle, session: &Arc<dyn Session>, text: &str, database: Option<&str>) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
    Ok(sink)
}

fn ctx() -> CallContext {
    CallContext::default()
}

async fn browse(handle: &Handle, request: BrowseRequest) -> AppResult<crate::contract::Page> {
    handle
        .call("browsed", |conn| async move {
            let tabular = conn
                .tabular()
                .ok_or_else(|| crate::contract::unsupported_capability("browsing rows"))?;
            tabular.browse(&request, &ctx()).await
        })
        .await
}

async fn apply(handle: &Handle, set: RowChangeSet) -> AppResult<ApplyResult> {
    handle
        .call("applied", |conn| async move {
            let tabular = conn
                .tabular()
                .ok_or_else(|| crate::contract::unsupported_capability("editing rows"))?;
            tabular.apply(&set, &ctx()).await
        })
        .await
}

fn cell<'a>(page: &'a crate::contract::Page, row: usize, column: &str) -> &'a Value {
    let index = page
        .columns
        .iter()
        .position(|c| c.name == column)
        .unwrap_or_else(|| panic!("no column {column}"));
    &page.rows[row][index]
}

fn text_of(value: &Value) -> String {
    value.display().unwrap_or_default()
}

/// Reads one row by key; `None` when it does not exist.
async fn read_row(f: &Fixture, handle: &Handle, key: i64) -> Option<Vec<Field>> {
    let mut request = BrowseRequest::new(f.table.clone(), 5);
    request.filter = (f.key_filter)(key);
    let page = browse(handle, request).await.expect("read row");
    page.rows.first().map(|row| {
        page.columns
            .iter()
            .zip(row)
            .map(|(column, value)| Field::new(&column.name, value.clone()))
            .collect()
    })
}

fn field<'a>(row: &'a [Field], column: &str) -> &'a Value {
    &row.iter()
        .find(|f| f.column == column)
        .unwrap_or_else(|| panic!("no field {column}"))
        .value
}

// --- Suites -------------------------------------------------------------------------------------------------------------

/// Classification is pure: samples per language, plus refusal of unknown operations.
pub fn suite_classify(f: &Fixture) {
    let engine = registry().get(f.engine).unwrap();
    for (text, expected) in f.classify_samples {
        let got = engine
            .classify(&ClassifyInput::Statement {
                language: f.language,
                text,
            })
            .worst();
        assert_eq!(got, Some(*expected), "{}: {text}", f.engine);
    }
    let unknown = engine.classify(&ClassifyInput::Capability {
        name: "contract.unknown",
        args: &serde_json::Value::Null,
    });
    assert!(unknown.refusal().is_some());
}

/// Connect, server info, capabilities within the descriptor, ping, close; a wrong password is a clear error.
pub async fn suite_connect(f: &Fixture) {
    let manager = ConnectionManager::default();
    let info = manager
        .connect(CONNECTION, f.spec.clone(), f.secrets.clone())
        .await
        .expect("connect");
    let descriptor = registry().get(f.engine).unwrap().descriptor();
    assert!(info.capabilities.is_subset_of(descriptor.capabilities), "{:?}", info.capabilities);
    assert!(!info.server.version.is_empty());
    assert!(!info.languages.is_empty());
    let handle = manager.handle(CONNECTION).unwrap();
    handle.call("pinged", |conn| async move { conn.ping().await }).await.expect("ping");
    manager.disconnect(CONNECTION).await;
    assert!(manager.handle(CONNECTION).is_err());
    if f.secrets.get(crate::contract::SecretSlot::Password).is_some() {
        let wrong = ConnectSecrets::password("definitely-not-the-password");
        let error = ConnectionManager::probe(f.spec.clone(), wrong)
            .await
            .expect_err("wrong password refused");
        assert!(!error.to_string().is_empty());
        assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some(f.engine), "{error:?}");
    }
}

/// The tree reaches the table from the top, and node keys are stable.
pub async fn suite_tree(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    let children = |parent| {
        let handle = handle.clone();
        async move {
            let request = TreeRequest {
                parent,
                ..TreeRequest::default()
            };
            handle
                .call("listed children", |conn| async move { conn.children(&request, &ctx()).await })
                .await
                .expect("children")
        }
    };
    let top = children(None).await;
    assert!(!top.nodes.is_empty());
    assert_eq!(top, children(None).await, "keys must be stable");
    // Breadth-first down to the table.
    let mut queue: Vec<_> = top.nodes;
    let mut found = None;
    for _ in 0..200 {
        let Some(node) = (!queue.is_empty()).then(|| queue.remove(0)) else {
            break;
        };
        if node
            .object
            .as_ref()
            .is_some_and(|o| o.name == f.table.name && o.database == f.table.database)
        {
            found = Some(node);
            break;
        }
        let wanted = node.kind == NodeKind::DATABASE && node.label.as_str() == f.table.database.as_deref().unwrap_or_default()
            || node.kind != NodeKind::DATABASE && node.expandable;
        if wanted {
            queue.extend(children(Some(node.key.clone())).await.nodes);
        }
    }
    let node = found.unwrap_or_else(|| panic!("{}: {} not reachable in the tree", f.engine, f.table.name));
    assert!(node.kind.is_relation());
    assert_eq!(node.default_view.as_deref(), Some("data"));
}

/// Columns, primary key, foreign keys (both directions).
pub async fn suite_describe(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    let describe = |object: ObjectRef| {
        let handle = handle.clone();
        async move {
            handle
                .call("described", |conn| async move { conn.describe(&object, &ctx()).await })
                .await
                .expect("describe")
        }
    };
    let items = describe(f.table.clone()).await;
    for column in [f.key_column, f.name_column, "price", "note"] {
        assert!(items.column(column).is_some(), "{}: missing column {column}", f.engine);
    }
    if !f.has_gap(Gap::TypedValues) {
        assert!(items.column("price").unwrap().kind.is_numeric(), "price must be numeric");
    }
    assert_eq!(
        items.primary_key.as_ref().map(|k| k.columns.clone()),
        Some(vec![f.key_column.to_owned()]),
        "{}: primary key",
        f.engine
    );
    assert!(items.column(f.key_column).unwrap().hints.primary);
    if handle.engine.descriptor().family == crate::contract::EngineFamily::Sql {
        let child = describe(f.child.clone()).await;
        assert!(
            child.foreign_keys.iter().any(|fk| fk.ref_table.ends_with(&f.table.name)),
            "{}: child foreign key",
            f.engine
        );
        if !f.has_gap(Gap::ReferencedBy) {
            assert!(
                items.referenced_by.iter().any(|r| r.table.ends_with(&f.child.name)),
                "{}: referenced by",
                f.engine
            );
        }
    }
}

/// Paging, sorting, totals, raw filters and every structured operator.
pub async fn suite_browse(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    let caps = handle.conn.capabilities();
    let mut request = BrowseRequest::new(f.table.clone(), 10);
    request.sort = vec![SortKey::asc(f.key_column)];
    request.estimate_total = true;
    let first = browse(&handle, request.clone()).await.expect("first page");
    assert_eq!(first.rows.len(), 10);
    assert!(first.has_more);
    assert_eq!(first.key_columns, vec![f.key_column.to_owned()]);
    assert_eq!(text_of(cell(&first, 0, f.key_column)), "1");
    if let Some(total) = first.total {
        assert!(total.value >= 900, "total {total:?}");
    }
    request.offset = 990;
    let last = browse(&handle, request.clone()).await.expect("last page");
    assert_eq!(last.rows.len(), 10);
    assert!(!last.has_more);
    request.offset = 0;
    request.sort = vec![SortKey::desc(f.key_column)];
    let desc = browse(&handle, request.clone()).await.expect("descending");
    assert_eq!(text_of(cell(&desc, 0, f.key_column)), "1000");
    if caps.contains(Capability::KeysetPaging) && !f.has_gap(Gap::Keyset) {
        request.sort = vec![SortKey::asc(f.key_column)];
        let page = browse(&handle, request.clone()).await.unwrap();
        let after = page.next_keyset.clone().expect("keyset");
        request.after = Some(after);
        let next = browse(&handle, request.clone()).await.unwrap();
        assert_eq!(text_of(cell(&next, 0, f.key_column)), "11");
        request.after = None;
    }
    if caps.contains(Capability::RawFilter) {
        let mut raw = BrowseRequest::new(f.table.clone(), 10);
        raw.filter = (f.key_filter)(7);
        let page = browse(&handle, raw).await.expect("raw filter");
        assert_eq!(page.rows.len(), 1);
    }
    if f.has_gap(Gap::StructuredFilter) {
        return;
    }
    let count = |op: FilterOp, column: &str, values: &[&str]| {
        let handle = handle.clone();
        let mut request = BrowseRequest::new(f.table.clone(), 2000);
        request.filter = Filter::Conditions {
            conditions: vec![FilterCond::new(column, op, values)],
            match_any: false,
        };
        async move { browse(&handle, request).await.map(|page| page.rows.len()) }
    };
    let k = f.key_column;
    let cases: Vec<(FilterOp, &str, Vec<&str>, usize)> = vec![
        (FilterOp::Eq, k, vec!["5"], 1),
        (FilterOp::Ne, k, vec!["5"], 999),
        (FilterOp::Lt, k, vec!["11"], 10),
        (FilterOp::Le, k, vec!["10"], 10),
        (FilterOp::Gt, k, vec!["990"], 10),
        (FilterOp::Ge, k, vec!["991"], 10),
        (FilterOp::Contains, f.name_column, vec!["ITEM 000"], 9),
        // "item 1000" is the only name without "item 0".
        (FilterOp::NotContains, f.name_column, vec!["item 0"], 1),
        (FilterOp::StartsWith, f.name_column, vec!["item 01"], 100),
        (FilterOp::EndsWith, f.name_column, vec!["999"], 1),
        (FilterOp::Like, f.name_column, vec!["item 00_1"], 10),
        (FilterOp::NotLike, f.name_column, vec!["item%"], 0),
        (FilterOp::In, k, vec!["1", "2", "3"], 3),
        (FilterOp::NotIn, k, vec!["1", "2", "3"], 997),
        (FilterOp::Between, k, vec!["10", "19"], 10),
        (FilterOp::IsNull, "note", vec![], 333),
        (FilterOp::IsNotNull, "note", vec![], 667),
    ];
    assert_eq!(cases.len(), FilterOp::ALL.len());
    for (op, column, values, expected) in cases {
        let got = count(op, column, &values)
            .await
            .unwrap_or_else(|e| panic!("{}: {op:?} failed: {e}", f.engine));
        assert_eq!(got, expected, "{}: {op:?} {column} {values:?}", f.engine);
    }
}

fn update(f: &Fixture, key: i64, original: &str, new: &str) -> RowChange {
    RowChange::Update {
        key: vec![Field::new(f.key_column, Value::Int(key))],
        values: vec![Field::new(f.name_column, Value::Text(new.to_owned()))],
        original: vec![Field::new(f.name_column, Value::Text(original.to_owned()))],
        version: None,
        force: false,
    }
}

fn set(f: &Fixture, changes: Vec<RowChange>) -> RowChangeSet {
    RowChangeSet {
        object: f.table.clone(),
        changes,
    }
}

fn insert(f: &Fixture, key: i64, name: &str) -> RowChange {
    RowChange::Insert {
        values: vec![
            Field::new(f.key_column, Value::Int(key)),
            Field::new(f.name_column, Value::Text(name.to_owned())),
            Field::new("price", Value::Decimal("9.99".into())),
            Field::new("active", Value::Bool(true)),
            Field::new("note", Value::Null),
        ],
    }
}

/// Insert, update and delete round trips, NULLs, and optimistic concurrency: a stale update or one on a deleted row is
/// a CONFLICT and nothing of the set is applied; "overwrite" (force) applies it.
pub async fn suite_edit(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    if !handle.conn.capabilities().contains(Capability::EditRows) {
        let error = apply(&handle, set(f, vec![])).await.unwrap_err();
        assert_eq!(error.code(), Some("UNSUPPORTED_CAPABILITY"));
        return;
    }
    let result = apply(&handle, set(f, vec![insert(f, 2001, "contract new")])).await.expect("insert");
    assert_eq!(result.inserted, 1);
    let row = read_row(f, &handle, 2001).await.expect("inserted row");
    assert_eq!(text_of(field(&row, f.name_column)), "contract new");
    assert!(field(&row, "note").is_null());
    let result = apply(&handle, set(f, vec![update(f, 2001, "contract new", "contract renamed")]))
        .await
        .expect("update");
    assert_eq!(result.updated, 1);
    assert_eq!(
        text_of(field(&read_row(f, &handle, 2001).await.unwrap(), f.name_column)),
        "contract renamed"
    );
    let delete = RowChange::Delete {
        key: vec![Field::new(f.key_column, Value::Int(2001))],
        original: vec![Field::new(f.name_column, Value::Text("contract renamed".into()))],
        version: None,
        force: false,
    };
    assert_eq!(apply(&handle, set(f, vec![delete])).await.expect("delete").deleted, 1);
    assert!(read_row(f, &handle, 2001).await.is_none());

    if f.has_gap(Gap::Concurrency) {
        return;
    }
    // Two writers edit the same row: B saves first; A's stale update is a conflict, and A's other change (an insert in
    // the same set) is rolled back.
    apply(&handle, set(f, vec![update(f, 10, "item 0010", "changed by B")]))
        .await
        .expect("writer B");
    let error = apply(
        &handle,
        set(f, vec![insert(f, 2002, "rolled back"), update(f, 10, "item 0010", "changed by A")]),
    )
    .await
    .expect_err("writer A conflicts");
    assert_conflict(f, &error, 1, Some("changed by B"), "changed by A");
    assert!(
        read_row(f, &handle, 2002).await.is_none(),
        "{}: the set must be rolled back",
        f.engine
    );
    assert_eq!(
        text_of(field(&read_row(f, &handle, 10).await.unwrap(), f.name_column)),
        "changed by B"
    );
    // Overwrite: the same change with force wins.
    let RowChange::Update { key, values, original, .. } = update(f, 10, "item 0010", "changed by A") else {
        unreachable!()
    };
    let forced = RowChange::Update {
        key,
        values,
        original,
        version: None,
        force: true,
    };
    apply(&handle, set(f, vec![forced])).await.expect("overwrite");
    assert_eq!(
        text_of(field(&read_row(f, &handle, 10).await.unwrap(), f.name_column)),
        "changed by A"
    );

    // The row is deleted before the apply: a conflict that says the row no longer exists.
    apply(&handle, set(f, vec![insert(f, 2003, "doomed")])).await.unwrap();
    let gone = RowChange::Delete {
        key: vec![Field::new(f.key_column, Value::Int(2003))],
        original: vec![],
        version: None,
        force: false,
    };
    apply(&handle, set(f, vec![gone])).await.unwrap();
    let error = apply(&handle, set(f, vec![update(f, 2003, "doomed", "too late")]))
        .await
        .expect_err("row gone");
    assert_conflict(f, &error, 0, None, "too late");
    // A stale delete is a conflict too.
    let stale_delete = RowChange::Delete {
        key: vec![Field::new(f.key_column, Value::Int(11))],
        original: vec![Field::new(f.name_column, Value::Text("not what it is".into()))],
        version: None,
        force: false,
    };
    let error = apply(&handle, set(f, vec![stale_delete])).await.expect_err("stale delete");
    assert_eq!(error.code(), Some(CONFLICT));
    assert!(read_row(f, &handle, 11).await.is_some());
}

fn assert_conflict(f: &Fixture, error: &AppError, index: usize, current_name: Option<&str>, attempted: &str) {
    assert_eq!(error.code(), Some(CONFLICT), "{}: {error}", f.engine);
    let conflicts = error.field("conflicts").and_then(|c| c.as_array()).expect("conflicts");
    let conflict = conflicts
        .iter()
        .find(|c| c["index"] == index)
        .unwrap_or_else(|| panic!("no conflict at {index}: {conflicts:?}"));
    let named = |list: &serde_json::Value| -> Option<String> {
        list.as_array()?
            .iter()
            .find(|field| field["column"] == f.name_column)
            .and_then(|field| field["value"]["v"].as_str().map(str::to_owned))
    };
    match current_name {
        Some(name) => assert_eq!(named(&conflict["current"]).as_deref(), Some(name), "{conflict}"),
        None => assert!(conflict["current"].is_null(), "{conflict}"),
    }
    assert_eq!(named(&conflict["attempted"]).as_deref(), Some(attempted));
}

/// Scripts with several statements, the row limit, and error mapping (syntax error, constraint violation).
pub async fn suite_query(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    let session = open_session(&handle).await;
    let database = f.table.database.as_deref();
    let sql = handle.engine.descriptor().family == crate::contract::EngineFamily::Sql;
    let sink = run_text(&handle, &session, f.multi_statement, database)
        .await
        .expect("multi-statement script");
    assert!(
        sink.sets.len() >= if sql { 2 } else { 1 },
        "{}: {} result sets",
        f.engine,
        sink.sets.len()
    );
    assert!(sink.sets.iter().any(|set| !set.rows.is_empty()));
    let mut limited = CollectSink::default();
    let request = RunRequest {
        text: format!("SELECT * FROM {}", qualified(f)),
        database: database.map(str::to_owned),
        offset: 0,
    };
    if sql {
        let options = RunOptions {
            row_limit: 100,
            batch_size: 30,
            ..RunOptions::default()
        };
        handle
            .run(Arc::clone(&session), request, options, &mut limited)
            .await
            .expect("limited run");
        assert_eq!(limited.sets[0].rows.len(), 100);
        assert!(limited.sets[0].truncated);
        assert!(limited.batches >= 1);
    }
    let syntax = run_text(&handle, &session, f.syntax_error, database)
        .await
        .expect_err("syntax error");
    assert!(!syntax.message().is_empty());
    let violation = run_text(&handle, &session, f.constraint_violation, database).await;
    if !f.has_gap(Gap::ErrorMapping) {
        let violation = violation.expect_err("constraint violation");
        for error in [&syntax, &violation] {
            assert_eq!(error.code(), Some("ENGINE_ERROR"), "{}: {error:?}", f.engine);
            assert!(error.field("vendorCode").is_some(), "{}: no vendor code in {error:?}", f.engine);
            assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some(f.engine));
        }
    }
    // The session still works after errors.
    run_text(&handle, &session, f.count_probe, database)
        .await
        .expect("session usable after errors");
    session.close().await;
}

fn qualified(f: &Fixture) -> String {
    match f.engine {
        "mssql" => format!("{}.dbo.{}", f.table.database.as_deref().unwrap_or_default(), f.table.name),
        "mongodb" => f.table.name.clone(),
        _ => format!("{}.{}", f.table.database.as_deref().unwrap_or_default(), f.table.name),
    }
}

/// A long statement is cancelled within 3 s and the session stays usable.
pub async fn suite_cancel(f: &Fixture) {
    let (manager, handle) = connect(f).await;
    let Some(sleep) = f.sleep_statement else { return };
    if !handle.conn.capabilities().contains(Capability::Cancel) {
        return;
    }
    manager
        .open_session(CONNECTION, "cancel".into(), SessionOptions::default())
        .await
        .expect("session");
    let (_, session) = manager.session("cancel").unwrap();
    let started = std::time::Instant::now();
    let runner = {
        let session = Arc::clone(&session);
        let handle = handle.clone();
        let sleep = sleep.to_owned();
        tokio::spawn(async move { run_text(&handle, &session, &sleep, None).await.map(|_| ()) })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    manager.cancel("cancel").await.expect("cancel");
    let outcome = tokio::time::timeout(Duration::from_secs(5), runner)
        .await
        .expect("run ended")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{}: took {:?}",
        f.engine,
        started.elapsed()
    );
    let _ = outcome;
    run_text(&handle, &session, f.count_probe, f.table.database.as_deref())
        .await
        .expect("usable after cancel");
}

/// Begin / Rollback undoes, Begin / Commit keeps, state is reported.
pub async fn suite_transactions(f: &Fixture) {
    let (_manager, handle) = connect(f).await;
    if !handle.conn.capabilities().contains(Capability::Transactions) {
        return;
    }
    let session = open_session(&handle).await;
    let database = f.table.database.as_deref();
    let options = RunOptions::default();
    let count = |session: Arc<dyn Session>| {
        let handle = handle.clone();
        async move {
            let sink = run_text(&handle, &session, f.count_probe, database).await.expect("count");
            text_of(&sink.sets.iter().find(|s| !s.rows.is_empty()).expect("count row").rows[0][0])
        }
    };
    let state = session.transaction(TransactionOp::Begin, &options).await.expect("begin");
    assert!(state.open);
    run_text(&handle, &session, f.insert_probe, database).await.expect("insert");
    session.transaction(TransactionOp::Rollback, &options).await.expect("rollback");
    assert_eq!(count(Arc::clone(&session)).await, "0");
    session.transaction(TransactionOp::Begin, &options).await.unwrap();
    run_text(&handle, &session, f.insert_probe, database).await.unwrap();
    let state = session.transaction(TransactionOp::Commit, &options).await.unwrap();
    assert!(!state.open);
    assert_eq!(count(Arc::clone(&session)).await, "1");
    session.close().await;
}

/// Generates the tests of one engine as a module named `contract_<engine>`, so `cargo test contract_<engine>` selects
/// them. The classification test always runs; the live suites are ignored by default: start the engine's compose file,
/// then `cargo test contract_<engine> -- --ignored --test-threads=1`.
#[macro_export]
macro_rules! contract_tests {
    ($name:ident, $fixture:expr) => {
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            use $crate::contract::testing::suites;

            #[test]
            fn classify() {
                suites::suite_classify(&$fixture);
            }

            $crate::contract_tests!(@live $fixture;
                suite_connect suite_tree suite_describe suite_browse suite_edit suite_query suite_cancel suite_transactions);
        }
    };
    (@live $fixture:expr; $($suite:ident)*) => {
        $(
            #[tokio::test(flavor = "multi_thread")]
            #[ignore = "needs the engine's container (dev/compose)"]
            async fn $suite() {
                suites::$suite(&$fixture).await;
            }
        )*
    };
}
