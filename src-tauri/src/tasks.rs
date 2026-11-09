use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    net::TcpStream,
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
};

use tokio::runtime::Handle;

use crate::{
    error::{AppError, AppResult},
    events::EventEmitter,
    job,
    lock::lock,
};

pub const TASKS_DISABLED: &str =
    "Startup tasks are turned off in this web edition. Set FOSTA_STUDIO_TASKS=on to run programs inside the container.";

const LOG_CAPACITY: usize = 2000;
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STABLE_RUN: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(100);
/// How long an exited process's remaining output may take to reach the log (a grandchild can keep the pipe open).
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const PORT_PROBE_INTERVAL: Duration = Duration::from_millis(200);
const PORT_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDef {
    pub id: String,
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "enabled")]
    pub auto_restart: bool,
    #[serde(default)]
    pub ready_pattern: Option<String>,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Stopped,
    Starting,
    Running,
    Restarting,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub id: String,
    pub state: TaskState,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub message: Option<String>,
}

impl TaskStatus {
    fn idle(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            state: TaskState::Stopped,
            pid: None,
            restarts: 0,
            message: None,
        }
    }
}

#[derive(Clone, Serialize)]
struct TaskLogEvent<'a> {
    id: &'a str,
    line: &'a str,
}

struct TaskRuntime {
    status: TaskStatus,
    logs: VecDeque<String>,
    stop: Option<watch::Sender<bool>>,
    /// The definition the supervisor runs, while it runs.
    running: Option<TaskDef>,
}

impl TaskRuntime {
    fn new(id: &str) -> Self {
        Self {
            status: TaskStatus::idle(id),
            logs: VecDeque::new(),
            stop: None,
            running: None,
        }
    }
}

/// When a started process counts as ready (state `running`) if its definition has no ready pattern.
#[derive(Clone, Debug, Default)]
pub enum ReadyCheck {
    /// As soon as it is spawned.
    #[default]
    Spawned,
    /// When `host:port` accepts a TCP connection.
    Port { host: String, port: u16 },
}

enum RunOutcome {
    Stopped,
    Ended(String),
}

pub struct TaskManager {
    /// Where the supervisors run (the front-end's tokio runtime, reachable from any thread).
    executor: Handle,
    /// Receives `task-status` and `task-log` events for the UI.
    events: Arc<dyn EventEmitter>,
    /// Off in the web edition unless the operator allows running programs inside the container.
    enabled: bool,
    runtimes: Mutex<HashMap<String, TaskRuntime>>,
}

impl TaskManager {
    pub fn new(executor: Handle, events: Arc<dyn EventEmitter>, enabled: bool) -> Self {
        Self {
            executor,
            events,
            enabled,
            runtimes: Mutex::default(),
        }
    }

    /// Starts every auto-start task that is not already running.
    pub fn start_auto(self: &Arc<Self>, defs: &[TaskDef]) {
        if !self.enabled {
            return;
        }
        for def in defs.iter().filter(|def| def.auto_start && !self.is_active(&def.id)) {
            if let Err(error) = self.start(def.clone()) {
                self.push_log(&def.id, &format!("✖ {error}"));
            }
        }
    }

    pub fn statuses(&self, defs: &[TaskDef]) -> Vec<TaskStatus> {
        let runtimes = lock(&self.runtimes);
        defs.iter()
            .map(|def| {
                runtimes
                    .get(&def.id)
                    .map_or_else(|| TaskStatus::idle(&def.id), |runtime| runtime.status.clone())
            })
            .collect()
    }

    pub fn logs(&self, id: &str) -> Vec<String> {
        lock(&self.runtimes)
            .get(id)
            .map(|runtime| runtime.logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn start(self: &Arc<Self>, def: TaskDef) -> AppResult<()> {
        self.start_with(def, ReadyCheck::Spawned)
    }

    /// Starts `def`; `ready` decides when it counts as running if it has no ready pattern.
    pub fn start_with(self: &Arc<Self>, def: TaskDef, ready: ReadyCheck) -> AppResult<()> {
        if !self.enabled {
            return Err(AppError::new(TASKS_DISABLED));
        }
        let stop_rx = self.with_runtime(&def.id, |runtime| {
            if runtime.stop.is_some() {
                return Err(AppError::new(format!("'{}' is already running", def.name)));
            }
            let (stop_tx, stop_rx) = watch::channel(false);
            runtime.stop = Some(stop_tx);
            runtime.running = Some(def.clone());
            runtime.status.state = TaskState::Starting;
            runtime.status.restarts = 0;
            runtime.status.message = None;
            Ok(stop_rx)
        })?;
        self.push_log(&def.id, &format!("▶ {} {}", def.program, def.args.join(" ")));
        self.executor.spawn(Arc::clone(self).supervise(def, ready, stop_rx));
        Ok(())
    }

    /// The status of one process, known or not.
    pub fn status(&self, id: &str) -> TaskStatus {
        lock(&self.runtimes)
            .get(id)
            .map_or_else(|| TaskStatus::idle(id), |runtime| runtime.status.clone())
    }

    /// The definition a process was started with, while its supervisor runs.
    pub fn running_def(&self, id: &str) -> Option<TaskDef> {
        lock(&self.runtimes).get(id).and_then(|runtime| runtime.running.clone())
    }

    /// Drops the status and log of a process that is not running (a temporary one).
    pub fn forget(&self, id: &str) {
        let mut runtimes = lock(&self.runtimes);
        if runtimes.get(id).is_some_and(|runtime| runtime.stop.is_none()) {
            runtimes.remove(id);
        }
    }

    pub fn stop(&self, id: &str) {
        if let Some(stop) = lock(&self.runtimes).get(id).and_then(|runtime| runtime.stop.as_ref()) {
            let _ = stop.send(true);
        }
    }

    pub fn stop_all(&self) {
        for stop in lock(&self.runtimes).values().filter_map(|runtime| runtime.stop.as_ref()) {
            let _ = stop.send(true);
        }
    }

    pub async fn restart(self: &Arc<Self>, def: TaskDef) -> AppResult<()> {
        self.stop(&def.id);
        self.wait_until_stopped(&def.id).await?;
        self.start(def)
    }

    pub(crate) async fn wait_until_stopped(&self, id: &str) -> AppResult<()> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.is_active(id) {
            if Instant::now() >= deadline {
                return Err(AppError::new("The task did not stop in time"));
            }
            tokio::time::sleep(STOP_POLL).await;
        }
        Ok(())
    }

    pub(crate) fn is_active(&self, id: &str) -> bool {
        lock(&self.runtimes).get(id).is_some_and(|runtime| runtime.stop.is_some())
    }

    async fn supervise(self: Arc<Self>, def: TaskDef, ready: ReadyCheck, mut stop_rx: watch::Receiver<bool>) {
        let mut backoff = MIN_BACKOFF;
        let (final_state, message) = loop {
            let started = Instant::now();
            let reason = match self.run_once(&def, &ready, &mut stop_rx).await {
                RunOutcome::Stopped => break (TaskState::Stopped, None),
                RunOutcome::Ended(reason) => reason,
            };
            if *stop_rx.borrow() {
                break (TaskState::Stopped, None);
            }
            if !def.auto_restart {
                break (TaskState::Failed, Some(reason));
            }
            let delay = if started.elapsed() >= STABLE_RUN { MIN_BACKOFF } else { backoff };
            backoff = (delay * 2).min(MAX_BACKOFF);
            self.update_runtime(&def.id, |runtime| {
                runtime.status.state = TaskState::Restarting;
                runtime.status.pid = None;
                runtime.status.restarts += 1;
                runtime.status.message = Some(format!("{reason}; retrying in {}s", delay.as_secs()));
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = stop_rx.changed() => break (TaskState::Stopped, None),
            }
        };
        self.update_runtime(&def.id, |runtime| {
            runtime.stop = None;
            runtime.running = None;
            runtime.status.state = final_state;
            runtime.status.pid = None;
            runtime.status.message = message;
        });
    }

    async fn run_once(self: &Arc<Self>, def: &TaskDef, ready: &ReadyCheck, stop_rx: &mut watch::Receiver<bool>) -> RunOutcome {
        let mut child = match spawn_process(def) {
            Ok(child) => child,
            Err(error) => {
                let reason = format!("cannot start '{}': {error}", def.program);
                self.push_log(&def.id, &format!("✖ {reason}"));
                return RunOutcome::Ended(reason);
            }
        };
        job::bind_to_app_lifetime(&child);
        let ready_pattern = def.ready_pattern.clone().filter(|pattern| !pattern.is_empty());
        let pid = child.id();
        let probe = match ready {
            ReadyCheck::Port { host, port } if ready_pattern.is_none() => Some((host.clone(), *port)),
            _ => None,
        };
        let initial_state = if ready_pattern.is_some() || probe.is_some() {
            TaskState::Starting
        } else {
            TaskState::Running
        };
        self.update_runtime(&def.id, |runtime| {
            runtime.status.state = initial_state;
            runtime.status.pid = pid;
            runtime.status.message = None;
        });
        let mut pumps = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            pumps.push(self.pump_output(&def.id, ready_pattern.clone(), stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            pumps.push(self.pump_output(&def.id, ready_pattern, stderr));
        }
        if let Some((host, port)) = probe {
            self.probe_port(&def.id, pid, host, port);
        }
        let exit = tokio::select! {
            exit = child.wait() => Some(exit),
            _ = stop_rx.changed() => None,
        };
        let Some(exit) = exit else {
            let _ = child.kill().await;
            self.push_log(&def.id, "■ stopped");
            return RunOutcome::Stopped;
        };
        // The last lines usually explain the exit: let them reach the log first.
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
            for pump in pumps {
                let _ = pump.await;
            }
        })
        .await;
        let reason = match exit {
            Ok(status) => format!("process exited ({status})"),
            Err(error) => format!("process failed: {error}"),
        };
        self.push_log(&def.id, &format!("■ {reason}"));
        RunOutcome::Ended(reason)
    }

    fn pump_output<R>(self: &Arc<Self>, id: &str, ready_pattern: Option<String>, reader: R) -> JoinHandle<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let manager = Arc::clone(self);
        let id = id.to_owned();
        self.executor.spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut buffer = Vec::new();
            while matches!(reader.read_until(b'\n', &mut buffer).await, Ok(read) if read > 0) {
                let line = String::from_utf8_lossy(&buffer).trim_end().to_owned();
                buffer.clear();
                if ready_pattern.as_deref().is_some_and(|pattern| line.contains(pattern)) {
                    manager.mark_ready(&id);
                }
                manager.push_log(&id, &line);
            }
        })
    }

    /// Marks the run with `pid` ready once `host:port` accepts a connection; gives up when that run is over.
    fn probe_port(self: &Arc<Self>, id: &str, pid: Option<u32>, host: String, port: u16) {
        let manager = Arc::clone(self);
        let id = id.to_owned();
        self.executor.spawn(async move {
            loop {
                let waiting = lock(&manager.runtimes)
                    .get(&id)
                    .is_some_and(|runtime| runtime.status.pid == pid && runtime.status.state == TaskState::Starting);
                if !waiting {
                    return;
                }
                let attempt = tokio::time::timeout(PORT_PROBE_TIMEOUT, TcpStream::connect((host.as_str(), port))).await;
                if matches!(attempt, Ok(Ok(_))) {
                    manager.mark_ready(&id);
                    return;
                }
                tokio::time::sleep(PORT_PROBE_INTERVAL).await;
            }
        });
    }

    fn mark_ready(&self, id: &str) {
        self.update_runtime(id, |runtime| {
            if runtime.status.state == TaskState::Starting {
                runtime.status.state = TaskState::Running;
            }
        });
    }

    fn push_log(&self, id: &str, line: &str) {
        self.with_runtime(id, |runtime| {
            if runtime.logs.len() == LOG_CAPACITY {
                runtime.logs.pop_front();
            }
            runtime.logs.push_back(line.to_owned());
        });
        self.events.emit("task-log", json!(TaskLogEvent { id, line }));
    }

    fn update_runtime(&self, id: &str, change: impl FnOnce(&mut TaskRuntime)) {
        let status = self.with_runtime(id, |runtime| {
            change(runtime);
            runtime.status.clone()
        });
        self.events.emit("task-status", json!(status));
    }

    fn with_runtime<R>(&self, id: &str, action: impl FnOnce(&mut TaskRuntime) -> R) -> R {
        let mut runtimes = lock(&self.runtimes);
        action(runtimes.entry(id.to_owned()).or_insert_with(|| TaskRuntime::new(id)))
    }
}

fn spawn_process(def: &TaskDef) -> std::io::Result<Child> {
    let mut command = Command::new(&def.program);
    command
        .args(&def.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn()
}
