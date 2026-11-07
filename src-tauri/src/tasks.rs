use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::watch,
};

use crate::{
    error::{AppError, AppResult},
    job,
    lock::lock,
};

const LOG_CAPACITY: usize = 2000;
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STABLE_RUN: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(100);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}

impl TaskRuntime {
    fn new(id: &str) -> Self {
        Self {
            status: TaskStatus::idle(id),
            logs: VecDeque::new(),
            stop: None,
        }
    }
}

enum RunOutcome {
    Stopped,
    Ended(String),
}

pub struct TaskManager {
    app: AppHandle,
    runtimes: Mutex<HashMap<String, TaskRuntime>>,
}

impl TaskManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            runtimes: Mutex::default(),
        }
    }

    /// Starts every auto-start task that is not already running.
    pub fn start_auto(self: &Arc<Self>, defs: &[TaskDef]) {
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

    pub fn start(self: &Arc<Self>, def: TaskDef) -> AppResult<()> {
        let stop_rx = self.with_runtime(&def.id, |runtime| {
            if runtime.stop.is_some() {
                return Err(AppError::new(format!("'{}' is already running", def.name)));
            }
            let (stop_tx, stop_rx) = watch::channel(false);
            runtime.stop = Some(stop_tx);
            runtime.status.restarts = 0;
            Ok(stop_rx)
        })?;
        self.push_log(&def.id, &format!("▶ {} {}", def.program, def.args.join(" ")));
        tauri::async_runtime::spawn(Arc::clone(self).supervise(def, stop_rx));
        Ok(())
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

    async fn wait_until_stopped(&self, id: &str) -> AppResult<()> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.is_active(id) {
            if Instant::now() >= deadline {
                return Err(AppError::new("The task did not stop in time"));
            }
            tokio::time::sleep(STOP_POLL).await;
        }
        Ok(())
    }

    fn is_active(&self, id: &str) -> bool {
        lock(&self.runtimes)
            .get(id)
            .is_some_and(|runtime| runtime.stop.is_some())
    }

    async fn supervise(self: Arc<Self>, def: TaskDef, mut stop_rx: watch::Receiver<bool>) {
        let mut backoff = MIN_BACKOFF;
        let (final_state, message) = loop {
            let started = Instant::now();
            let reason = match self.run_once(&def, &mut stop_rx).await {
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
            runtime.status.state = final_state;
            runtime.status.pid = None;
            runtime.status.message = message;
        });
    }

    async fn run_once(self: &Arc<Self>, def: &TaskDef, stop_rx: &mut watch::Receiver<bool>) -> RunOutcome {
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
        let initial_state = if ready_pattern.is_some() { TaskState::Starting } else { TaskState::Running };
        self.update_runtime(&def.id, |runtime| {
            runtime.status.state = initial_state;
            runtime.status.pid = pid;
            runtime.status.message = None;
        });
        if let Some(stdout) = child.stdout.take() {
            self.pump_output(&def.id, ready_pattern.clone(), stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            self.pump_output(&def.id, ready_pattern, stderr);
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
        let reason = match exit {
            Ok(status) => format!("process exited ({status})"),
            Err(error) => format!("process failed: {error}"),
        };
        self.push_log(&def.id, &format!("■ {reason}"));
        RunOutcome::Ended(reason)
    }

    fn pump_output<R>(self: &Arc<Self>, id: &str, ready_pattern: Option<String>, reader: R)
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let manager = Arc::clone(self);
        let id = id.to_owned();
        tauri::async_runtime::spawn(async move {
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
        let _ = self.app.emit("task-log", TaskLogEvent { id, line });
    }

    fn update_runtime(&self, id: &str, change: impl FnOnce(&mut TaskRuntime)) {
        let status = self.with_runtime(id, |runtime| {
            change(runtime);
            runtime.status.clone()
        });
        let _ = self.app.emit("task-status", status);
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
