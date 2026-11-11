//! Value sampling against an in-memory engine: a SQL flavour that answers its distinct-values statements from fixed
//! columns, and a MongoDB flavour that answers `mongo.distinct`.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::{
    ai::{
        profile::{learn, store::ProfileStore},
        AiRuns,
    },
    contract::{
        testing::fake, CallContext, CapabilitySet, Category, Classification, ClassifyInput, ConnectRequest, Connection, Engine,
        EngineDescriptor, EngineFamily, KeyInfo, NodeKey, ObjectDescription, ObjectNode, OpenedSession, ResultSink, RunOptions, RunRequest,
        ServerInfo, Session, SessionOptions, StatementInfo, StreamEvent, TreeChildren, TreeRequest, CANCELLED,
    },
    gateway::rules::{Action, Preset, SafetySettings},
};

/// The engine: `mongo` answers `mongo.distinct`, otherwise SQL statements `DISTINCT <column> FROM <table> LIMIT <n>`.
struct Engine2 {
    mongo: bool,
}

fn descriptor(mongo: bool) -> EngineDescriptor {
    let mut descriptor = fake::descriptor();
    if mongo {
        descriptor.id = MONGODB.into();
        descriptor.family = EngineFamily::Document;
    }
    descriptor.capabilities = CapabilitySet::of(&[Capability::Tree, Capability::Describe, Capability::Query]);
    descriptor
}

#[async_trait]
impl Engine for Engine2 {
    fn descriptor(&self) -> &EngineDescriptor {
        static SQL: OnceLock<EngineDescriptor> = OnceLock::new();
        static MONGO: OnceLock<EngineDescriptor> = OnceLock::new();
        if self.mongo {
            MONGO.get_or_init(|| descriptor(true))
        } else {
            SQL.get_or_init(|| descriptor(false))
        }
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        match input {
            ClassifyInput::Statement { text, .. } if text.starts_with("SELECT") => {
                Classification::single(StatementInfo::new(Category::Read, "SELECT", text))
            }
            ClassifyInput::Capability {
                name: "mongo.distinct", ..
            } => Classification::single(StatementInfo::new(Category::Read, "mongo.distinct", "mongo.distinct")),
            _ => Classification::single(StatementInfo::new(Category::Write, "WRITE", "write")),
        }
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        Err(crate::error::AppError::new("not used"))
    }

    fn distinct_values_statement(&self, object: &ObjectRef, column: &ColumnInfo, sample: u64, limit: u64) -> Option<String> {
        (!self.mongo).then(|| format!("SELECT DISTINCT {} FROM {} SAMPLE {sample} LIMIT {limit}", column.name, object.name))
    }
}

/// The tables: `orders` with a few kinds of columns, and `wide` with 40 short text columns.
type Columns = Vec<(String, &'static str, ValueKind)>;

fn tables() -> Vec<(&'static str, Columns)> {
    let orders = vec![
        ("id".to_owned(), "int", ValueKind::Int),
        ("status".to_owned(), "text", ValueKind::Text),
        ("email".to_owned(), "text", ValueKind::Text),
        ("password".to_owned(), "text", ValueKind::Text),
        ("level".to_owned(), "smallint", ValueKind::Int),
        ("note".to_owned(), "text", ValueKind::Text),
        ("total".to_owned(), "bigint", ValueKind::Int),
        ("slow".to_owned(), "text", ValueKind::Text),
    ];
    let wide = (0..40).map(|index| (format!("c{index:02}"), "text", ValueKind::Text)).collect();
    vec![("orders", orders), ("wide", wide)]
}

/// The values a column holds (at most the asked limit come back).
fn values_of(column: &str) -> Vec<String> {
    match column {
        "status" => vec!["new".into(), "paid".into(), "shipped".into()],
        "email" => vec!["a@example.com".into(), "b@example.org".into()],
        "level" => vec!["1".into(), "2".into()],
        "note" => (0..30).map(|index| format!("note {index}")).collect(),
        "password" => vec!["hunter2".into()],
        _ => vec!["x".into()],
    }
}

struct State {
    statements: Mutex<Vec<String>>,
    /// The `slow` column never answers.
    slow: AtomicBool,
}

struct Conn {
    engine: &'static Engine2,
    state: Arc<State>,
    info: ServerInfo,
}

fn node(id: &str, label: &str, kind: NodeKind, object: Option<ObjectRef>) -> ObjectNode {
    ObjectNode {
        key: NodeKey {
            id: id.into(),
            kind: kind.clone(),
            object: object.clone(),
        },
        label: label.into(),
        expandable: !kind.is_relation(),
        kind,
        icon: "table".into(),
        object,
        detail: None,
        badges: Vec::new(),
        default_view: None,
        actions: Vec::new(),
    }
}

fn kind(engine: &Engine2) -> NodeKind {
    if engine.mongo {
        NodeKind::COLLECTION
    } else {
        NodeKind::TABLE
    }
}

#[async_trait]
impl Connection for Conn {
    fn engine_id(&self) -> &str {
        self.engine.descriptor().id.as_str()
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.engine.descriptor().capabilities
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let nodes = match request.parent {
            None => vec![node("db:shop", "shop", NodeKind::DATABASE, None)],
            Some(_) => tables()
                .into_iter()
                .map(|(name, _)| {
                    let object = ObjectRef::new(Some("shop"), None, name, kind(self.engine));
                    node(name, name, kind(self.engine), Some(object))
                })
                .collect(),
        };
        Ok(TreeChildren {
            nodes,
            next_cursor: None,
            total_hint: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let (_, columns) = tables().into_iter().find(|(name, _)| *name == object.name).unwrap();
        let mut description = ObjectDescription::new(object.clone());
        description.columns = columns
            .iter()
            .map(|(name, type_name, kind)| ColumnInfo::result(name, type_name, kind.clone()))
            .collect();
        if object.name == "orders" {
            description.primary_key = Some(KeyInfo {
                name: None,
                columns: vec!["id".into()],
            });
        }
        Ok(description)
    }

    async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        Ok(OpenedSession {
            session: Arc::new(Sess(Arc::clone(&self.state))),
            canceller: None,
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        assert_eq!(name, "mongo.distinct");
        let field = args["field"].as_str().unwrap().to_owned();
        self.state.statements.lock().unwrap().push(format!("distinct {field}"));
        let limit = args["limit"].as_u64().unwrap() as usize;
        let values = values_of(&field);
        Ok(json!({ "values": values.iter().take(limit).collect::<Vec<_>>(), "truncated": values.len() > limit }))
    }

    async fn close(&self) {}
}

struct Sess(Arc<State>);

#[async_trait]
impl Session for Sess {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, _options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        self.0.statements.lock().unwrap().push(request.text.clone());
        let words: Vec<&str> = request.text.split(' ').collect();
        let (column, limit) = (words[2], words.last().unwrap().parse::<usize>().unwrap());
        if column == "slow" && self.0.slow.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        sink.event(StreamEvent::Columns {
            index: 0,
            columns: vec![ColumnInfo::result(column, "text", ValueKind::Text)],
        });
        let rows: Vec<Vec<Value>> = values_of(column)
            .into_iter()
            .take(limit)
            .map(|value| vec![Value::Text(value)])
            .collect();
        let count = rows.len();
        sink.event(StreamEvent::Rows { index: 0, rows });
        sink.event(StreamEvent::Done {
            index: 0,
            affected: None,
            rows: count,
            truncated: false,
            elapsed_ms: 0,
        });
        Ok(())
    }
}

fn setup(mongo: bool) -> (ConnectionManager, Handle, Arc<State>) {
    let engine: &'static Engine2 = Box::leak(Box::new(Engine2 { mongo }));
    let state = Arc::new(State {
        statements: Mutex::new(Vec::new()),
        slow: AtomicBool::new(false),
    });
    let manager = ConnectionManager::default();
    manager.insert(
        "c",
        engine,
        Arc::new(Conn {
            engine,
            state: Arc::clone(&state),
            info: ServerInfo::default(),
        }),
    );
    let handle = manager.handle("c").unwrap();
    (manager, handle, state)
}

fn context(safety: SafetySettings) -> Context {
    Context {
        workspace_id: "w".into(),
        workspace_name: "Team".into(),
        connection_id: "c".into(),
        connection_name: "Main".into(),
        safety,
    }
}

fn store(name: &str) -> (Arc<ProfileStore>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("fosta-values-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Arc::new(ProfileStore::new(dir.clone())), dir)
}

fn quiet(_: Progress) {}

fn hints(profile: &DatabaseProfile, object: &str) -> HashMap<String, ValueHint> {
    profile
        .object(object)
        .unwrap()
        .value_hints
        .iter()
        .map(|hint| (hint.column.clone(), hint.clone()))
        .collect()
}

fn open() -> Context {
    context(SafetySettings::preset(Preset::Open))
}

fn read_columns(state: &State) -> Vec<String> {
    state
        .statements
        .lock()
        .unwrap()
        .iter()
        .map(|statement| {
            statement
                .split(' ')
                .nth(if statement.starts_with("distinct") { 1 } else { 2 })
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn sql_hints_are_read_when_allowed_within_the_caps() {
    let (manager, handle, state) = setup(false);
    let (store, dir) = store("sql");
    let runs = AiRuns::default();
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let steps = Mutex::new(Vec::new());
    let record = |progress: Progress| steps.lock().unwrap().push(progress.step);
    let outcome = learn(&runs, &store, &handle, "shop", "r", Some(sampler), &record).await.unwrap();
    let profile = store.load("c", "shop").unwrap().unwrap();
    assert!(profile.data_read && outcome.status.data_read);
    let orders = hints(&profile, "orders");
    assert_eq!(orders["status"].values, ["new", "paid", "shipped"]);
    assert_eq!(orders["level"].values, ["1", "2"]);
    // Emails give their format, never the addresses; a column with many values gives nothing.
    assert_eq!(
        (orders["email"].format.as_deref(), orders["email"].values.is_empty()),
        (Some("email"), true)
    );
    assert!(!orders.contains_key("note"));
    // Never the key, the sensitive column or a wide integer; at most 30 columns of `wide`.
    let columns = read_columns(&state);
    for skipped in ["id", "password", "total"] {
        assert!(!columns.iter().any(|column| column == skipped), "{skipped}: {columns:?}");
    }
    let statements = state.statements.lock().unwrap().clone();
    assert_eq!(
        statements.iter().filter(|s| s.contains("FROM wide")).count(),
        MAX_COLUMNS_PER_OBJECT
    );
    assert!(
        statements.iter().all(|s| s.ends_with(&format!("SAMPLE {SAMPLE_ROWS} LIMIT 21"))),
        "{statements:?}"
    );
    let values = outcome.values.unwrap();
    assert_eq!((values.columns, values.failed, values.skipped), (statements.len(), 0, None));
    assert_eq!(
        values.hinted,
        profile.objects.iter().map(|object| object.value_hints.len()).sum::<usize>()
    );
    assert!(steps.lock().unwrap().contains(&Step::Values));
    // The view for a chat that may not read data now.
    let mut view = profile.clone();
    strip_value_hints(&mut view);
    assert!(view.objects.iter().all(|object| object.value_hints.is_empty()));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn nothing_is_read_without_a_sampler_or_when_the_rules_refuse() {
    let (manager, handle, state) = setup(false);
    let (store, dir) = store("off");
    let runs = AiRuns::default();
    let outcome = learn(&runs, &store, &handle, "shop", "r", None, &quiet).await.unwrap();
    assert!(outcome.values.is_none() && !outcome.status.data_read);
    assert!(state.statements.lock().unwrap().is_empty());
    let profile = store.load("c", "shop").unwrap().unwrap();
    assert!(profile.objects.iter().all(|object| object.value_hints.is_empty()));
    // Rules that confirm reads refuse the assistant: sampling stops at the first read, and nothing reaches a session.
    let mut confirm = SafetySettings::preset(Preset::Custom);
    confirm.rules.read = Action::Confirm;
    let sampler = Sampler {
        manager: &manager,
        context: context(confirm),
    };
    let outcome = learn(&runs, &store, &handle, "shop", "r", Some(sampler), &quiet).await.unwrap();
    let values = outcome.values.unwrap();
    assert!(values.skipped.unwrap().contains("refused"));
    assert_eq!((values.columns, values.hinted), (1, 0));
    assert!(state.statements.lock().unwrap().is_empty());
    assert!(!outcome.status.data_read);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_cancelled_sample_stops_the_build() {
    let (manager, handle, state) = setup(false);
    state.slow.store(true, Ordering::Relaxed);
    let (store, dir) = store("cancel");
    let runs = AiRuns::default();
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let started = Instant::now();
    let (outcome, ()) = tokio::join!(learn(&runs, &store, &handle, "shop", "r", Some(sampler), &quiet), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        runs.cancel("r");
    });
    let error = outcome.unwrap_err();
    assert_eq!(error.code(), Some(CANCELLED), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(read_columns(&state).contains(&"slow".to_owned()));
    assert!(store.load("c", "shop").unwrap().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mongo_hints_come_from_distinct() {
    let (manager, handle, state) = setup(true);
    let (store, dir) = store("mongo");
    let runs = AiRuns::default();
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let outcome = learn(&runs, &store, &handle, "shop", "r", Some(sampler), &quiet).await.unwrap();
    let profile = store.load("c", "shop").unwrap().unwrap();
    let orders = hints(&profile, "orders");
    assert_eq!(orders["status"].values, ["new", "paid", "shipped"]);
    assert!(!orders.contains_key("note"));
    let statements = state.statements.lock().unwrap().clone();
    assert!(
        statements.iter().all(|statement| statement.starts_with("distinct ")),
        "{statements:?}"
    );
    assert!(!read_columns(&state).iter().any(|column| column == "password" || column == "id"));
    assert!(outcome.values.unwrap().hinted >= 3 && profile.data_read);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn engines_without_a_bounded_read_are_skipped() {
    let manager = ConnectionManager::default();
    let engine = fake::FakeEngine::leak();
    manager.insert("c", engine, engine.connection());
    let handle = manager.handle("c").unwrap();
    let token = CancellationToken::new();
    let mut profile = super::super::build::build(&handle, "c", "shop", &token, &quiet).await.unwrap();
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let outcome = sample(&sampler, &handle, &mut profile, &token, &quiet).await.unwrap();
    assert!(outcome.skipped.unwrap().contains("no bounded way"));
    assert_eq!((outcome.columns, profile.data_read), (0, false));
    assert!(!engine.calls.lock().unwrap().iter().any(|call| call == "run" || call == "invoke"));
}

/// Against the MongoDB container (`db-studio-dev-mongo-1`, root / studio-root on 127.0.0.1:27017).
#[tokio::test]
#[ignore = "needs the MongoDB container (dev/compose); run with --ignored --test-threads=1"]
async fn live_mongo_hints() {
    use crate::contract::{CollectSink, ConnectSecrets, ConnectionSpec};
    let env = |name: &str, default: &str| std::env::var(format!("FOSTA_CONTRACT_MONGO_{name}")).unwrap_or_else(|_| default.into());
    let spec = ConnectionSpec {
        engine: MONGODB.into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "27017").parse().unwrap_or(27017),
        user: env("USER", "root"),
        database: Some("admin".into()),
        ..ConnectionSpec::default()
    };
    let manager = ConnectionManager::default();
    manager
        .connect("c", spec, ConnectSecrets::password(&env("PASSWORD", "studio-root")))
        .await
        .expect("connect");
    let handle = manager.handle("c").unwrap();
    const STATUSES: [&str; 3] = ["new", "paid", "shipped"];
    let documents: Vec<serde_json::Value> = (0..40usize)
        .map(|index| {
            json!({
                "_id": index,
                "status": STATUSES[index % 3],
                "email": format!("user{index}@example.com"),
                "password": format!("secret-{index}"),
                "note": format!("note {index}"),
                "active": index % 2 == 0,
            })
        })
        .collect();
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .unwrap()
        .session;
    for command in [
        json!({ "delete": "orders", "deletes": [{ "q": {}, "limit": 0 }] }),
        json!({ "insert": "orders", "documents": documents }),
    ] {
        let request = RunRequest {
            text: command.to_string(),
            database: Some("fosta_values".into()),
            offset: 0,
        };
        let mut sink = CollectSink::default();
        handle
            .run(Arc::clone(&session), request, RunOptions::default(), &mut sink)
            .await
            .unwrap();
    }
    let (store, dir) = store("live-mongo");
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let outcome = learn(&AiRuns::default(), &store, &handle, "fosta_values", "r", Some(sampler), &quiet)
        .await
        .unwrap();
    let profile = store.load("c", "fosta_values").unwrap().unwrap();
    let orders = hints(&profile, "orders");
    assert_eq!(orders["status"].values, ["new", "paid", "shipped"]);
    assert_eq!(orders["active"].values, ["false", "true"]);
    assert_eq!(orders["email"].format.as_deref(), Some("email"));
    assert!(orders["email"].values.is_empty());
    assert!(!orders.contains_key("password") && !orders.contains_key("note") && !orders.contains_key("_id"));
    assert!(profile.data_read && outcome.values.unwrap().failed == 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_engine_classifies_its_distinct_values_statement_as_one_read() {
    let registry = crate::runtime::registry::registry();
    let object = ObjectRef::new(Some("shop"), Some("pub\"lic"), "ord`ers]", NodeKind::TABLE);
    let column = ColumnInfo::result("st\"a`tus]", "text", ValueKind::Text);
    let mut covered = Vec::new();
    for id in registry.ids() {
        let engine = registry.get(id).unwrap();
        let Some(statement) = engine.distinct_values_statement(&object, &column, SAMPLE_ROWS, 21) else {
            continue;
        };
        let language = engine.descriptor().default_language().unwrap().id.clone();
        let classification = engine.classify(&ClassifyInput::Statement {
            language: &language,
            text: &statement,
        });
        assert_eq!(classification.statements.len(), 1, "{id}: {statement}");
        assert_eq!(classification.worst(), Some(Category::Read), "{id}: {statement}");
        assert!(classification.refusal().is_none(), "{id}: {statement}");
        covered.push(id);
    }
    assert!(covered.len() >= 6, "{covered:?}");
}

/// A real SQL engine end to end: a SQLite file in the temporary directory.
#[tokio::test(flavor = "multi_thread")]
async fn sqlite_hints_through_the_real_engine() {
    use crate::contract::{CollectSink, ConnectSecrets, ConnectionSpec};
    let file = std::env::temp_dir().join(format!("fosta-values-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut spec = ConnectionSpec {
        engine: "sqlite".into(),
        file: Some(file.to_string_lossy().into_owned()),
        ..ConnectionSpec::default()
    };
    spec.options.insert("create".into(), "true".into());
    let manager = ConnectionManager::default();
    manager.connect("c", spec, ConnectSecrets::default()).await.expect("connect");
    let handle = manager.handle("c").unwrap();
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .unwrap()
        .session;
    let mut statements = vec![
        "CREATE TABLE \"order items\" (id INTEGER PRIMARY KEY, \"st\"\"atus\" TEXT, email TEXT, password TEXT, note TEXT, level SMALLINT)"
            .to_owned(),
    ];
    for index in 0..40 {
        statements.push(format!(
            "INSERT INTO \"order items\" VALUES ({index}, '{}', 'user{index}@example.com', 'pw{index}', 'note {index}', {})",
            ["new", "paid"][index % 2],
            index % 3
        ));
    }
    for text in statements {
        let request = RunRequest {
            text,
            database: None,
            offset: 0,
        };
        let mut sink = CollectSink::default();
        handle
            .run(Arc::clone(&session), request, RunOptions::default(), &mut sink)
            .await
            .unwrap();
    }
    let (store, dir) = store("sqlite");
    let sampler = Sampler {
        manager: &manager,
        context: open(),
    };
    let outcome = learn(&AiRuns::default(), &store, &handle, "main", "r", Some(sampler), &quiet)
        .await
        .unwrap();
    let values = outcome.values.unwrap();
    let profile = store.load("c", "main").unwrap().unwrap();
    let hints = hints(&profile, "order items");
    assert_eq!(hints["st\"atus"].values, ["new", "paid"], "{values:?}");
    assert_eq!(hints["level"].values, ["0", "1", "2"]);
    assert_eq!(hints["email"].format.as_deref(), Some("email"));
    assert!(!hints.contains_key("password") && !hints.contains_key("note") && !hints.contains_key("id"));
    assert_eq!((values.columns, values.failed), (4, 0), "{values:?}");
    assert!(profile.data_read);
    manager.disconnect("c").await;
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir_all(dir);
}
