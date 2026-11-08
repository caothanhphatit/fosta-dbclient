//! The connection manager: one map of open connections and one map of query sessions, every engine call run through
//! crash containment. Replaces `drivers::DbManager`.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tokio::sync::mpsc;

use serde::Serialize;

use super::{
    contain::{contained, crashed, is_crash},
    registry::registry,
};
use crate::{
    contract::{
        Canceller, ConnectInfo, ConnectRequest, ConnectSecrets, Connection, ConnectionSpec, Engine, Flow, ResultSink, RunOptions,
        RunRequest, RunSummary, Session, SessionInfo, SessionOptions, StreamEvent,
    },
    drivers::Driver,
    error::{AppError, AppResult, NOT_CONNECTED},
    gateway::Gateway,
    lock::lock,
    logging, net,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const SESSION_LOST: &str = "This query tab lost its session. Close it and open a new query.";

struct ConnEntry {
    engine: &'static dyn Engine,
    conn: Arc<dyn Connection>,
    poisoned: Arc<AtomicBool>,
    info: ConnectInfo,
    /// The pre-contract driver behind a legacy connection; the old commands use it until they are removed.
    legacy: Option<Arc<dyn Driver>>,
    _tunnel: Option<net::TunnelGuard>,
}

enum SessionKind {
    Contract {
        session: Arc<dyn Session>,
        canceller: Option<Arc<dyn Canceller>>,
    },
    /// A session the old commands opened on the legacy driver under the tab's session id.
    Legacy,
}

struct SessionEntry {
    connection_id: String,
    kind: SessionKind,
}

/// A cheap handle on an open connection for one call.
#[derive(Clone)]
pub struct Handle {
    pub engine: &'static dyn Engine,
    pub conn: Arc<dyn Connection>,
    pub connection_id: String,
    poisoned: Arc<AtomicBool>,
}

impl Handle {
    /// Runs one engine call in its own task. A panic becomes `ENGINE_CRASHED` and marks the connection crashed; every
    /// error gets the engine id.
    pub async fn call<T, F, Fut>(&self, what: &'static str, call: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn Connection>) -> Fut,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let label = self.engine.descriptor().label.clone();
        let outcome = contained(&label, what, call(Arc::clone(&self.conn))).await;
        if outcome.as_ref().err().is_some_and(is_crash) {
            self.poisoned.store(true, Ordering::Relaxed);
        }
        outcome.map_err(|error| error.with_engine(self.engine.id()))
    }

    /// Runs a session's statements in their own task (contained like [`call`](Self::call)) and forwards the events to
    /// `sink` as they come (the row limit bounds what is buffered). When `sink` answers `Stop` (the client went away),
    /// the engine's sink answers `Stop` too and the run ends at the next batch.
    pub async fn run(
        &self,
        session: Arc<dyn Session>,
        request: RunRequest,
        options: RunOptions,
        sink: &mut dyn ResultSink,
    ) -> AppResult<RunSummary> {
        struct ChannelSink {
            tx: mpsc::UnboundedSender<StreamEvent>,
            stop: Arc<AtomicBool>,
        }
        impl ResultSink for ChannelSink {
            fn event(&mut self, event: StreamEvent) -> Flow {
                if self.stop.load(Ordering::Relaxed) {
                    return Flow::Stop;
                }
                match self.tx.send(event) {
                    Ok(()) if !self.stop.load(Ordering::Relaxed) => Flow::Continue,
                    _ => Flow::Stop,
                }
            }
        }
        let started = Instant::now();
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamEvent>();
        let stop = Arc::new(AtomicBool::new(false));
        let mut engine_sink = ChannelSink {
            tx,
            stop: Arc::clone(&stop),
        };
        let task_session = Arc::clone(&session);
        let label = self.engine.descriptor().label.clone();
        let run = contained(&label, "ran statements", async move {
            task_session.run(&request, &options, &mut engine_sink).await
        });
        tokio::pin!(run);
        let mut statements = 0;
        let mut stopped = false;
        let outcome = loop {
            tokio::select! {
                biased;
                event = rx.recv() => match event {
                    Some(event) => {
                        if matches!(event, StreamEvent::Statement { .. }) {
                            statements += 1;
                        }
                        if sink.event(event) == Flow::Stop {
                            stopped = true;
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                    None => break (&mut run).await,
                },
                outcome = &mut run => {
                    // Drain what the engine sent before it finished.
                    while let Ok(event) = rx.try_recv() {
                        if matches!(event, StreamEvent::Statement { .. }) {
                            statements += 1;
                        }
                        if !stopped && sink.event(event) == Flow::Stop {
                            stopped = true;
                        }
                    }
                    break outcome;
                }
            }
        };
        if outcome.as_ref().err().is_some_and(is_crash) {
            self.poisoned.store(true, Ordering::Relaxed);
        }
        outcome.map_err(|error| error.with_engine(self.engine.id()))?;
        Ok(RunSummary {
            statements,
            elapsed_ms: started.elapsed().as_millis() as u64,
            stopped,
            transaction: session.transaction_state().await,
        })
    }
}

/// An open connection as reported by `diagnostics()`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConnection {
    pub engine: String,
    pub version: String,
    pub crashed: bool,
}

#[derive(Default)]
pub struct ConnectionManager {
    connections: Mutex<HashMap<String, ConnEntry>>,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    /// Checks every operation against the workspace's rules (see `crate::gateway`).
    pub gateway: Gateway,
}

/// Connects without registering: the engine from the registry, the tunnel from `net::prepare`, contained.
async fn open(spec: ConnectionSpec, secrets: ConnectSecrets) -> AppResult<(&'static dyn Engine, Opened)> {
    let engine = registry().get(&spec.engine)?;
    let prepared = net::prepare(&spec, &secrets).await?;
    let request = ConnectRequest {
        spec,
        secrets,
        endpoint: prepared.endpoint,
        connect_timeout: CONNECT_TIMEOUT,
    };
    let label = engine.descriptor().label.clone();
    let id = engine.id().to_owned();
    let legacy = super::legacy::handles(&id);
    let (conn, driver) = contained(&label, "connected", async move {
        if legacy {
            let (conn, driver) = super::legacy::connect(&request).await?;
            Ok((conn, Some(driver)))
        } else {
            Ok((engine.connect(request).await?, None))
        }
    })
    .await
    .map_err(|error| error.with_engine(&id))?;
    let info = connect_info(engine, conn.as_ref());
    Ok((
        engine,
        Opened {
            conn,
            info,
            legacy: driver,
            tunnel: prepared.tunnel,
        },
    ))
}

struct Opened {
    conn: Arc<dyn Connection>,
    info: ConnectInfo,
    legacy: Option<Arc<dyn Driver>>,
    tunnel: Option<net::TunnelGuard>,
}

fn connect_info(engine: &'static dyn Engine, conn: &dyn Connection) -> ConnectInfo {
    let languages = match conn.languages() {
        languages if languages.is_empty() => engine.descriptor().languages.clone(),
        languages => languages,
    };
    ConnectInfo {
        engine: engine.id().to_owned(),
        server: conn.info().clone(),
        capabilities: conn.capabilities().intersect(engine.descriptor().capabilities),
        languages,
        connection_actions: conn.connection_actions(),
    }
}

impl ConnectionManager {
    /// Connects `id` (replacing an earlier connection with that id) and reports what the server offers.
    pub async fn connect(&self, id: &str, spec: ConnectionSpec, secrets: ConnectSecrets) -> AppResult<ConnectInfo> {
        let engine_id = spec.engine.clone();
        let (engine, opened) = open(spec, secrets).await?;
        tracing::info!(engine = engine_id.as_str(), connection = logging::id_hash(id), version = %opened.info.server.version, "connected");
        let info = opened.info.clone();
        let entry = ConnEntry {
            engine,
            conn: opened.conn,
            poisoned: Arc::default(),
            info: opened.info,
            legacy: opened.legacy,
            _tunnel: opened.tunnel,
        };
        let previous = lock(&self.connections).insert(id.to_owned(), entry);
        if let Some(previous) = previous {
            self.close_entry(previous).await;
        }
        Ok(info)
    }

    /// "Test connection": connects, reads what the server offers, closes.
    pub async fn probe(spec: ConnectionSpec, secrets: ConnectSecrets) -> AppResult<ConnectInfo> {
        let (_, opened) = open(spec, secrets).await?;
        opened.conn.close().await;
        Ok(opened.info)
    }

    async fn close_entry(&self, entry: ConnEntry) {
        let conn = entry.conn;
        let label = entry.engine.descriptor().label.clone();
        let _ = contained(&label, "disconnected", async move {
            conn.close().await;
            Ok(())
        })
        .await;
    }

    /// Closes the connection's sessions, then the connection.
    pub async fn disconnect(&self, connection_id: &str) {
        let sessions: Vec<SessionEntry> = {
            let mut map = lock(&self.sessions);
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, entry)| entry.connection_id == connection_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        };
        for entry in sessions {
            if let SessionKind::Contract { session, .. } = entry.kind {
                session.close().await;
            }
        }
        let removed = lock(&self.connections).remove(connection_id);
        if let Some(entry) = removed {
            self.close_entry(entry).await;
        }
    }

    /// The connection, unless it is not open (`NOT_CONNECTED`) or crashed earlier (`ENGINE_CRASHED`).
    pub fn handle(&self, connection_id: &str) -> AppResult<Handle> {
        let connections = lock(&self.connections);
        let entry = connections.get(connection_id).ok_or_else(|| AppError::new(NOT_CONNECTED))?;
        if entry.poisoned.load(Ordering::Relaxed) {
            let label = &entry.engine.descriptor().label;
            return Err(crashed(label, "ran an earlier operation", "the connection is closed after a crash").with_engine(entry.engine.id()));
        }
        Ok(Handle {
            engine: entry.engine,
            conn: Arc::clone(&entry.conn),
            connection_id: connection_id.to_owned(),
            poisoned: Arc::clone(&entry.poisoned),
        })
    }

    pub fn connect_info(&self, connection_id: &str) -> AppResult<ConnectInfo> {
        lock(&self.connections)
            .get(connection_id)
            .map(|entry| entry.info.clone())
            .ok_or_else(|| AppError::new(NOT_CONNECTED))
    }

    /// Opens a contract session for a query tab.
    pub async fn open_session(&self, connection_id: &str, session_id: String, options: SessionOptions) -> AppResult<SessionInfo> {
        let handle = self.handle(connection_id)?;
        let ctx = crate::contract::CallContext::default();
        let opened = handle
            .call("opened a query session", move |conn| async move {
                conn.open_session(&options, &ctx).await
            })
            .await?;
        let info = SessionInfo {
            session_id: session_id.clone(),
            language: opened.session.language().to_owned(),
            transaction: opened.session.transaction_state().await,
            cancellable: opened.canceller.is_some(),
        };
        let previous = lock(&self.sessions).insert(
            session_id,
            SessionEntry {
                connection_id: connection_id.to_owned(),
                kind: SessionKind::Contract {
                    session: opened.session,
                    canceller: opened.canceller,
                },
            },
        );
        if let Some(SessionEntry {
            kind: SessionKind::Contract { session, .. },
            ..
        }) = previous
        {
            session.close().await;
        }
        Ok(info)
    }

    /// The handle and session of a query tab.
    pub fn session(&self, session_id: &str) -> AppResult<(Handle, Arc<dyn Session>)> {
        let (connection_id, session) = {
            let sessions = lock(&self.sessions);
            let entry = sessions.get(session_id).ok_or_else(|| AppError::new(SESSION_LOST))?;
            match &entry.kind {
                SessionKind::Contract { session, .. } => (entry.connection_id.clone(), Arc::clone(session)),
                SessionKind::Legacy => return Err(AppError::new(SESSION_LOST)),
            }
        };
        Ok((self.handle(&connection_id)?, session))
    }

    /// The connection a query-tab session belongs to.
    pub fn session_connection(&self, session_id: &str) -> AppResult<String> {
        lock(&self.sessions)
            .get(session_id)
            .map(|entry| entry.connection_id.clone())
            .ok_or_else(|| AppError::new(SESSION_LOST))
    }

    pub async fn close_session(&self, session_id: &str) {
        let removed = lock(&self.sessions).remove(session_id);
        match removed {
            Some(SessionEntry {
                kind: SessionKind::Contract { session, .. },
                ..
            }) => session.close().await,
            Some(SessionEntry {
                connection_id,
                kind: SessionKind::Legacy,
            }) => {
                if let Ok(driver) = self.legacy_driver(&connection_id) {
                    driver.close_session(session_id);
                }
            }
            None => {}
        }
    }

    /// Cancels the statement a session runs (without waiting for the session's lock).
    pub async fn cancel(&self, session_id: &str) -> AppResult<()> {
        let (connection_id, canceller, legacy) = {
            let sessions = lock(&self.sessions);
            let entry = sessions.get(session_id).ok_or_else(|| AppError::new(SESSION_LOST))?;
            match &entry.kind {
                SessionKind::Contract { canceller, .. } => (entry.connection_id.clone(), canceller.clone(), false),
                SessionKind::Legacy => (entry.connection_id.clone(), None, true),
            }
        };
        if legacy {
            let id = session_id.to_owned();
            return self
                .call_legacy(&connection_id, "cancelled a query", |driver| async move {
                    driver.cancel_query(&id).await
                })
                .await;
        }
        let canceller = canceller.ok_or_else(|| AppError::coded("UNSUPPORTED_CAPABILITY", "Stop is not supported by this engine."))?;
        let handle = self.handle(&connection_id)?;
        handle.call("cancelled a query", move |_| canceller.cancel()).await
    }

    /// Engine and server version of every open connection, for `diagnostics()`.
    pub fn open_connections(&self) -> Vec<OpenConnection> {
        lock(&self.connections)
            .values()
            .map(|entry| OpenConnection {
                engine: entry.engine.id().to_owned(),
                version: entry.info.server.version.clone(),
                crashed: entry.poisoned.load(Ordering::Relaxed),
            })
            .collect()
    }

    // --- The pre-contract path, for the old commands until they are removed -------------------------------------------

    /// The legacy driver of a connection (the assistant tools use it until they move to the contract).
    pub fn legacy_driver(&self, connection_id: &str) -> AppResult<Arc<dyn Driver>> {
        self.handle(connection_id)?;
        lock(&self.connections)
            .get(connection_id)
            .and_then(|entry| entry.legacy.clone())
            .ok_or_else(|| AppError::new("This connection does not support this operation in this build."))
    }

    /// The legacy driver's engine kind (classification of the old commands).
    pub fn legacy_engine(&self, connection_id: &str) -> AppResult<crate::drivers::Engine> {
        Ok(self.legacy_driver(connection_id)?.engine())
    }

    /// Runs one call on the legacy driver, contained like [`Handle::call`].
    pub async fn call_legacy<T, F, Fut>(&self, connection_id: &str, what: &'static str, call: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn Driver>) -> Fut,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let handle = self.handle(connection_id)?;
        let driver = self.legacy_driver(connection_id)?;
        handle.call(what, move |_| call(driver)).await
    }

    /// [`call_legacy`](Self::call_legacy) on the connection a legacy session belongs to.
    pub async fn call_legacy_session<T, F, Fut>(&self, session_id: &str, what: &'static str, call: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn Driver>) -> Fut,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let connection_id = self.session_connection(session_id)?;
        self.call_legacy(&connection_id, what, call).await
    }

    /// Opens a session on the legacy driver under the tab's own id.
    pub async fn open_legacy_session(&self, connection_id: &str, session_id: String) -> AppResult<()> {
        let id = session_id.clone();
        self.call_legacy(connection_id, "opened a query session", |driver| async move {
            driver.open_session(&id).await
        })
        .await?;
        lock(&self.sessions).insert(
            session_id,
            SessionEntry {
                connection_id: connection_id.to_owned(),
                kind: SessionKind::Legacy,
            },
        );
        Ok(())
    }

    /// A pooled MariaDB/MySQL connection for the vector tooling (until it moves to `invoke`).
    pub async fn mysql_conn(&self, connection_id: &str) -> AppResult<mysql_async::Conn> {
        self.call_legacy(connection_id, "opened a connection", |driver| async move {
            let mysql = driver.as_mysql().ok_or_else(|| AppError::new(crate::drivers::NEEDS_MYSQL))?;
            mysql.pooled().await
        })
        .await
    }

    pub fn is_mysql(&self, connection_id: &str) -> AppResult<bool> {
        Ok(self.legacy_driver(connection_id)?.as_mysql().is_some())
    }

    /// Registers a legacy driver directly (tests use fakes that record what they were asked to do).
    #[cfg(test)]
    pub fn insert_legacy(&self, id: &str, driver: Arc<dyn Driver>) {
        let engine = super::legacy::engine_for(driver.engine());
        let conn = super::legacy::wrap(Arc::clone(&driver), "test");
        self.insert(id, engine, conn, Some(driver));
    }

    /// Registers a connection directly (tests use fakes).
    #[cfg(test)]
    pub fn insert(&self, id: &str, engine: &'static dyn Engine, conn: Arc<dyn Connection>, legacy: Option<Arc<dyn Driver>>) {
        let info = connect_info(engine, conn.as_ref());
        lock(&self.connections).insert(
            id.to_owned(),
            ConnEntry {
                engine,
                conn,
                poisoned: Arc::default(),
                info,
                legacy,
                _tunnel: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::contract::{testing::FakeEngine, BrowseRequest, CallContext, CollectSink, ObjectRef, RunOptions, RunRequest, TreeRequest};

    fn browse() -> BrowseRequest {
        BrowseRequest::new(ObjectRef::table("shop", None, "items"), 10)
    }

    async fn read(manager: &ConnectionManager, id: &str) -> AppResult<crate::contract::Page> {
        manager
            .handle(id)?
            .call("read a page of rows", |conn| async move {
                conn.tabular().expect("tabular").browse(&browse(), &CallContext::default()).await
            })
            .await
    }

    #[tokio::test]
    async fn a_panicking_engine_call_is_contained_and_poisons_only_that_connection() {
        let manager = ConnectionManager::default();
        let bad = FakeEngine::leak();
        let good = FakeEngine::leak();
        manager.insert("bad", bad, bad.connection(), None);
        manager.insert("good", good, good.connection(), None);
        bad.panics.browse.store(true, Ordering::Relaxed);
        let error = read(&manager, "bad").await.expect_err("panicked");
        assert!(is_crash(&error), "{error}");
        assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some("fake"));
        // Every later call on that connection is refused until it reconnects; other connections keep working.
        bad.panics.browse.store(false, Ordering::Relaxed);
        assert!(is_crash(&manager.handle("bad").err().expect("poisoned")));
        assert!(manager.open_connections().iter().any(|c| c.crashed));
        assert_eq!(read(&manager, "good").await.unwrap().rows.len(), 1);
        manager.disconnect("bad").await;
        assert_eq!(manager.handle("bad").err().map(|e| e.to_string()), Some(NOT_CONNECTED.to_owned()));
    }

    #[tokio::test]
    async fn sessions_run_through_the_manager_and_close_with_their_connection() {
        let manager = ConnectionManager::default();
        let engine = FakeEngine::leak();
        manager.insert("c", engine, engine.connection(), None);
        let info = manager.open_session("c", "tab".into(), SessionOptions::default()).await.unwrap();
        assert_eq!(info.language, "sql");
        assert!(!info.cancellable);
        let (handle, session) = manager.session("tab").unwrap();
        assert_eq!(handle.connection_id, "c");
        let mut sink = CollectSink::default();
        let request = RunRequest {
            text: "SELECT 1".into(),
            ..RunRequest::default()
        };
        session.run(&request, &RunOptions::default(), &mut sink).await.unwrap();
        assert_eq!(sink.sets[0].sql, "SELECT 1");
        assert_eq!(manager.cancel("tab").await.unwrap_err().code(), Some("UNSUPPORTED_CAPABILITY"));
        let children = handle
            .call("listed the tree", |conn| async move {
                conn.children(&TreeRequest::default(), &CallContext::default()).await
            })
            .await
            .unwrap();
        assert_eq!(children.nodes[0].label, "shop");
        manager.disconnect("c").await;
        assert!(manager.session("tab").is_err());
    }

    #[tokio::test]
    async fn a_panic_while_connecting_is_contained_and_unknown_engines_are_refused() {
        let engine = FakeEngine::leak();
        engine.panics.connect.store(true, Ordering::Relaxed);
        let request = ConnectRequest {
            spec: ConnectionSpec::default(),
            secrets: ConnectSecrets::default(),
            endpoint: crate::contract::Endpoint { host: "x".into(), port: 1 },
            connect_timeout: CONNECT_TIMEOUT,
        };
        let error = contained("Fake", "connected", async move { engine.connect(request).await.map(|_| ()) })
            .await
            .expect_err("panicked");
        assert!(is_crash(&error));
        let spec = ConnectionSpec {
            engine: "postgress".into(),
            ..ConnectionSpec::default()
        };
        let Err(error) = ConnectionManager::probe(spec, ConnectSecrets::default()).await else {
            panic!("refused")
        };
        assert_eq!(error.code(), Some("UNKNOWN_ENGINE"));
    }
}
