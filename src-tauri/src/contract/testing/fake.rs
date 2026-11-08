//! An in-memory engine for core tests: one database `shop` with a table `items(id, name)`, a session that echoes
//! statements, and switches that make chosen operations panic.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};

use async_trait::async_trait;

use crate::{
    contract::{
        classify_common, ApplyResult, BrowseRequest, CallContext, Capability, CapabilitySet, Category, Classification, ClassifyInput,
        ColumnInfo, ConnectRequest, Connection, ConnectionForm, Engine, EngineDescriptor, EngineFamily, EngineStatus, FilterSupport,
        LanguageSpec, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, OpenedSession, Page, ResultSink, RowChangeSet,
        RunOptions, RunRequest, ServerInfo, Session, SessionOptions, StatementInfo, StreamEvent, TabularData, TreeChildren, TreeRequest,
        Value, ValueKind,
    },
    error::AppResult,
};

/// Which operation panics.
#[derive(Default)]
pub struct Panics {
    pub connect: AtomicBool,
    pub browse: AtomicBool,
    pub run: AtomicBool,
}

pub struct FakeEngine {
    pub panics: Panics,
    pub calls: Mutex<Vec<String>>,
}

impl FakeEngine {
    /// A fresh engine for one test (leaked: engines are static).
    pub fn leak() -> &'static FakeEngine {
        Box::leak(Box::new(FakeEngine {
            panics: Panics::default(),
            calls: Mutex::new(Vec::new()),
        }))
    }

    /// A connection of this engine, as connect returns it.
    pub fn connection(&'static self) -> Arc<dyn Connection> {
        Arc::new(FakeConnection {
            engine: self,
            info: ServerInfo::default(),
        })
    }

    fn record(&self, what: &str) {
        self.calls.lock().unwrap().push(what.to_owned());
    }
}

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: "fake".into(),
        label: "Fake".into(),
        family: EngineFamily::Sql,
        icon: "database".into(),
        status: EngineStatus::Ready,
        sql_dialect: None,
        capabilities: CapabilitySet::of(&[Capability::Tree, Capability::Describe, Capability::BrowseRows, Capability::Query]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(1),
            default_user: String::new(),
            database_label: String::new(),
            database_hint: String::new(),
            url: None,
            auth: Vec::new(),
            fields: Vec::new(),
            ssh: false,
            tls: None,
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: None,
            default: true,
            hint: String::new(),
        }],
        filter: FilterSupport {
            structured: true,
            raw: None,
            placeholder: String::new(),
        },
    }
}

#[async_trait]
impl Engine for FakeEngine {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        classify_common(input).unwrap_or_else(|| match input {
            ClassifyInput::Statement { text, .. } if text.to_ascii_lowercase().starts_with("select") => {
                Classification::single(StatementInfo::new(Category::Read, "SELECT", text))
            }
            ClassifyInput::Statement { text, .. } => Classification::single(StatementInfo::new(Category::Write, "WRITE", text)),
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        })
    }

    async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        self.record("connect");
        if self.panics.connect.load(Ordering::Relaxed) {
            panic!("fake connect panicked");
        }
        Ok(FakeEngine::connection(Box::leak(Box::new(FakeEngine {
            panics: Panics::default(),
            calls: Mutex::new(Vec::new()),
        }))))
    }
}

pub struct FakeConnection {
    engine: &'static FakeEngine,
    info: ServerInfo,
}

fn items() -> ObjectRef {
    ObjectRef::table("shop", None, "items")
}

#[async_trait]
impl Connection for FakeConnection {
    fn engine_id(&self) -> &str {
        "fake"
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.engine.descriptor().capabilities
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let node = |id: &str, label: &str, kind: NodeKind, object: Option<ObjectRef>, expandable: bool| ObjectNode {
            key: NodeKey {
                id: id.into(),
                kind: kind.clone(),
                object: object.clone(),
            },
            label: label.into(),
            kind,
            icon: "table".into(),
            object,
            detail: None,
            badges: Vec::new(),
            expandable,
            default_view: None,
            actions: Vec::new(),
        };
        let nodes = match &request.parent {
            None => vec![node("db:shop", "shop", NodeKind::DATABASE, None, true)],
            Some(_) => vec![node("db:shop/table:items", "items", NodeKind::TABLE, Some(items()), false)],
        };
        Ok(TreeChildren {
            nodes,
            next_cursor: None,
            total_hint: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let mut description = ObjectDescription::new(object.clone());
        description.columns = vec![
            ColumnInfo::result("id", "int", ValueKind::Int),
            ColumnInfo::result("name", "text", ValueKind::Text),
        ];
        Ok(description)
    }

    async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        Ok(OpenedSession {
            session: Arc::new(FakeSession { engine: self.engine }),
            canceller: None,
        })
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    async fn close(&self) {}
}

#[async_trait]
impl TabularData for FakeConnection {
    async fn browse(&self, _request: &BrowseRequest, _ctx: &CallContext) -> AppResult<Page> {
        if self.engine.panics.browse.load(Ordering::Relaxed) {
            panic!("fake browse panicked");
        }
        Ok(Page {
            columns: vec![ColumnInfo::result("id", "int", ValueKind::Int)],
            rows: vec![vec![Value::Int(1)]],
            has_more: false,
            key_columns: vec!["id".into()],
            next_keyset: None,
            total: None,
            statement: None,
            elapsed_ms: 0,
            row_versions: None,
        })
    }

    async fn apply(&self, _changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        Ok(ApplyResult::default())
    }
}

struct FakeSession {
    engine: &'static FakeEngine,
}

#[async_trait]
impl Session for FakeSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, _options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        if self.engine.panics.run.load(Ordering::Relaxed) {
            panic!("fake run panicked");
        }
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: request.text.clone(),
        });
        sink.event(StreamEvent::Done {
            index: 0,
            affected: Some(0),
            rows: 0,
            truncated: false,
            elapsed_ms: 0,
        });
        Ok(())
    }
}
