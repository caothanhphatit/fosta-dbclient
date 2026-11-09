//! A command bound to one connection (a "tunnel command", typically `kubectl port-forward`): started before the
//! connection connects and stopped when it disconnects. It runs in the [`TaskManager`] like a startup task (same
//! spawning, log, restart with backoff, events), under the id `tunnel:<connection id>`.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    tasks::{ReadyCheck, TaskDef, TaskManager, TaskState},
};

/// The error code of a tunnel command that did not become ready.
pub const TUNNEL_FAILED: &str = "TUNNEL_FAILED";
pub const TUNNELS_DISABLED: &str =
    "Tunnel commands are turned off in this web edition. Set FOSTA_STUDIO_TASKS=on to run programs inside the container.";

const DEFAULT_READY_TIMEOUT_SECS: u64 = 30;
const MAX_READY_TIMEOUT_SECS: u64 = 600;
const LOG_TAIL: usize = 15;
const READY_POLL: Duration = Duration::from_millis(100);

static NEXT_PROBE: AtomicU64 = AtomicU64::new(1);

/// The command a connection runs while it is connected. Not a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Ready when the output contains this; without it, when the connection's host:port accepts connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_pattern: Option<String>,
    #[serde(default = "default_timeout")]
    pub ready_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_READY_TIMEOUT_SECS
}

impl TunnelConfig {
    /// Trimmed, with an empty pattern dropped; `None` when no program is set.
    pub fn normalized(&self) -> Option<Self> {
        let program = self.program.trim();
        if program.is_empty() {
            return None;
        }
        Some(Self {
            program: program.to_owned(),
            args: self.args.clone(),
            ready_pattern: self
                .ready_pattern
                .as_deref()
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(str::to_owned),
            ready_timeout_secs: self.ready_timeout_secs,
        })
    }

    /// Whether `other` runs the same program with the same arguments.
    pub fn same_command(&self, other: &TunnelConfig) -> bool {
        self.program.trim() == other.program.trim() && self.args == other.args
    }

    fn timeout(&self) -> Duration {
        let secs = match self.ready_timeout_secs {
            0 => DEFAULT_READY_TIMEOUT_SECS,
            secs => secs.min(MAX_READY_TIMEOUT_SECS),
        };
        Duration::from_secs(secs)
    }
}

/// The task id of a connection's tunnel.
pub fn task_id(connection_id: &str) -> String {
    format!("tunnel:{connection_id}")
}

/// When a tunnel without a ready pattern is ready: when the connection's endpoint accepts TCP connections.
pub fn ready_check(host: &str, port: u16) -> ReadyCheck {
    if port == 0 {
        return ReadyCheck::Spawned;
    }
    let host = host.trim();
    ReadyCheck::Port {
        host: if host.is_empty() { "127.0.0.1" } else { host }.to_owned(),
        port,
    }
}

fn task_def(id: &str, name: &str, tunnel: &TunnelConfig) -> TaskDef {
    TaskDef {
        id: id.to_owned(),
        name: format!("Tunnel · {name}"),
        program: tunnel.program.clone(),
        args: tunnel.args.clone(),
        auto_start: false,
        auto_restart: true,
        ready_pattern: tunnel.ready_pattern.clone(),
    }
}

/// Whether a running tunnel runs the wanted command (once ready, its ready pattern no longer matters).
fn runs(running: &TaskDef, wanted: &TaskDef) -> bool {
    running.program == wanted.program && running.args == wanted.args
}

/// Starts the tunnel `id` (or reuses it when it already runs the same command) and waits until it is ready.
/// Returns whether it was started by this call. A tunnel that does not become ready is stopped again.
pub async fn ensure(tasks: &Arc<TaskManager>, id: &str, name: &str, tunnel: &TunnelConfig, ready: ReadyCheck) -> AppResult<bool> {
    if !tasks.enabled() {
        return Err(AppError::new(TUNNELS_DISABLED));
    }
    let def = task_def(id, name, tunnel);
    if let Some(running) = tasks.running_def(id) {
        if runs(&running, &def) {
            if let Err(error) = wait_ready(tasks, id, tunnel.timeout(), false).await {
                stop(tasks, id).await;
                return Err(error);
            }
            return Ok(false);
        }
        stop(tasks, id).await;
    }
    tasks.start_with(def, ready)?;
    if let Err(error) = wait_ready(tasks, id, tunnel.timeout(), true).await {
        stop(tasks, id).await;
        return Err(error);
    }
    Ok(true)
}

/// Stops the tunnel `id` and waits for its process to end.
pub async fn stop(tasks: &TaskManager, id: &str) {
    if tasks.is_active(id) {
        tasks.stop(id);
        let _ = tasks.wait_until_stopped(id).await;
    }
}

/// Waits until the tunnel is running. `fresh`: it was just started, so any exit fails (rather than waiting for the
/// restart).
async fn wait_ready(tasks: &TaskManager, id: &str, timeout: Duration, fresh: bool) -> AppResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = tasks.status(id);
        match status.state {
            TaskState::Running if !(fresh && status.restarts > 0) => return Ok(()),
            TaskState::Failed | TaskState::Stopped => return Err(failure(tasks, id, "exited before it was ready")),
            TaskState::Restarting if fresh => return Err(failure(tasks, id, "exited before it was ready")),
            _ if fresh && status.restarts > 0 => return Err(failure(tasks, id, "exited before it was ready")),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(failure(tasks, id, &format!("was not ready after {}s", timeout.as_secs())));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

fn failure(tasks: &TaskManager, id: &str, what: &str) -> AppError {
    let logs = tasks.logs(id);
    let tail = logs[logs.len().saturating_sub(LOG_TAIL)..].join("\n");
    let message = if tail.is_empty() {
        format!("The tunnel command {what}.")
    } else {
        format!("The tunnel command {what}. Last output:\n{tail}")
    };
    AppError::coded(TUNNEL_FAILED, message)
}

/// A tunnel used by "Test connection"; [`ProbeTunnel::finish`] stops it unless it was the connection's own.
pub struct ProbeTunnel {
    temporary: Option<String>,
}

/// Readies a tunnel for a probe of `connection_id`'s draft profile: the connection's running tunnel when it runs the
/// same command (a second port-forward on the same local port would fail), else a temporary one.
pub async fn start_probe(
    tasks: &Arc<TaskManager>,
    connection_id: &str,
    name: &str,
    tunnel: &TunnelConfig,
    ready: ReadyCheck,
) -> AppResult<ProbeTunnel> {
    let own = task_id(connection_id);
    if tasks
        .running_def(&own)
        .is_some_and(|running| runs(&running, &task_def(&own, name, tunnel)))
    {
        ensure(tasks, &own, name, tunnel, ready).await?;
        return Ok(ProbeTunnel { temporary: None });
    }
    let id = format!("tunnel-probe:{}", NEXT_PROBE.fetch_add(1, Ordering::Relaxed));
    match ensure(tasks, &id, name, tunnel, ready).await {
        Ok(_) => Ok(ProbeTunnel { temporary: Some(id) }),
        Err(error) => {
            tasks.forget(&id);
            Err(error)
        }
    }
}

impl ProbeTunnel {
    pub async fn finish(self, tasks: &TaskManager) {
        if let Some(id) = self.temporary {
            stop(tasks, &id).await;
            tasks.forget(&id);
        }
    }
}

/// Small cross-platform commands for tests (Windows and Linux CI).
#[cfg(test)]
pub(crate) mod testing {
    use super::TunnelConfig;

    pub(crate) fn tunnel(program: &str, args: &[&str], pattern: Option<&str>, timeout: u64) -> TunnelConfig {
        TunnelConfig {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ready_pattern: pattern.map(str::to_owned),
            ready_timeout_secs: timeout,
        }
    }

    /// A process that prints one line and keeps running (no grandchild, so stopping it ends it), and that line.
    pub(crate) fn long_running(pattern: Option<&str>) -> (TunnelConfig, &'static str) {
        if cfg!(windows) {
            (tunnel("ping", &["-n", "30", "127.0.0.1"], pattern, 10), "Pinging 127.0.0.1")
        } else {
            (
                tunnel("sh", &["-c", "echo Forwarding from 127.0.0.1; exec sleep 30"], pattern, 10),
                "Forwarding from",
            )
        }
    }

    /// A process that prints "boom: port is in use" and exits with code 3.
    pub(crate) fn exits_early() -> TunnelConfig {
        if cfg!(windows) {
            tunnel("cmd", &["/C", "echo boom: port is in use& exit /b 3"], Some("never"), 10)
        } else {
            tunnel("sh", &["-c", "echo boom: port is in use; exit 3"], Some("never"), 10)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{exits_early, long_running, tunnel};
    use super::*;
    use crate::events::NoEvents;
    use std::net::TcpListener;

    fn manager() -> Arc<TaskManager> {
        Arc::new(TaskManager::new(tokio::runtime::Handle::current(), Arc::new(NoEvents), true))
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn normalizing_trims_and_drops_an_empty_tunnel() {
        assert_eq!(tunnel("  ", &[], None, 30).normalized(), None);
        let normalized = tunnel(" kubectl ", &["port-forward"], Some("  "), 30).normalized().unwrap();
        assert_eq!(normalized.program, "kubectl");
        assert_eq!(normalized.ready_pattern, None);
        assert!(normalized.same_command(&tunnel("kubectl", &["port-forward"], Some("Forwarding"), 5)));
        assert!(!normalized.same_command(&tunnel("kubectl", &["port-forward", "svc/b"], None, 30)));
        assert_eq!(tunnel("x", &[], None, 0).timeout(), Duration::from_secs(30));
    }

    #[test]
    fn the_timeout_defaults_to_30_seconds_in_json() {
        let parsed: TunnelConfig = serde_json::from_str(r#"{"program":"kubectl"}"#).unwrap();
        assert_eq!(parsed.ready_timeout_secs, 30);
        assert!(parsed.args.is_empty());
    }

    #[tokio::test]
    async fn ready_when_the_output_contains_the_pattern() {
        let tasks = manager();
        let (config, line) = long_running(None);
        let config = TunnelConfig {
            ready_pattern: Some(line.to_owned()),
            ..config
        };
        let id = task_id("pattern");
        assert!(ensure(&tasks, &id, "db", &config, ReadyCheck::Spawned).await.unwrap());
        assert_eq!(tasks.status(&id).state, TaskState::Running);
        assert!(tasks.logs(&id).iter().any(|logged| logged.contains(line)));
        stop(&tasks, &id).await;
    }

    #[tokio::test]
    async fn ready_when_the_port_accepts_connections() {
        let tasks = manager();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (config, _) = long_running(None);
        let id = task_id("port");
        assert!(ensure(&tasks, &id, "db", &config, ready_check("", port)).await.unwrap());
        assert_eq!(tasks.status(&id).state, TaskState::Running);
        stop(&tasks, &id).await;
    }

    #[tokio::test]
    async fn a_closed_port_times_out_and_stops_the_tunnel() {
        let tasks = manager();
        let (config, line) = long_running(None);
        let config = TunnelConfig {
            ready_timeout_secs: 1,
            ..config
        };
        let id = task_id("closed");
        let error = ensure(&tasks, &id, "db", &config, ready_check("127.0.0.1", free_port()))
            .await
            .unwrap_err();
        assert_eq!(error.field("code").and_then(|code| code.as_str()), Some(TUNNEL_FAILED));
        assert!(error.to_string().contains("was not ready after 1s"), "{error}");
        assert!(error.to_string().contains(line), "{error}");
        assert!(!tasks.is_active(&id));
    }

    #[tokio::test]
    async fn an_early_exit_fails_with_the_log_tail() {
        let tasks = manager();
        let id = task_id("early");
        let error = ensure(&tasks, &id, "db", &exits_early(), ReadyCheck::Spawned).await.unwrap_err();
        assert_eq!(error.field("code").and_then(|code| code.as_str()), Some(TUNNEL_FAILED));
        let message = error.to_string();
        assert!(message.contains("exited before it was ready"), "{message}");
        assert!(message.contains("boom: port is in use"), "{message}");
        assert!(!tasks.is_active(&id), "a failed tunnel is not restarted in the background");
    }

    #[tokio::test]
    async fn a_missing_program_fails() {
        let tasks = manager();
        let config = tunnel("fosta-no-such-program", &[], None, 5);
        let error = ensure(&tasks, &task_id("missing"), "db", &config, ReadyCheck::Spawned)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot start 'fosta-no-such-program'"), "{error}");
    }

    #[tokio::test]
    async fn a_running_tunnel_is_reused_and_stopped_on_request() {
        let tasks = manager();
        let (config, _) = long_running(None);
        let id = task_id("reuse");
        assert!(ensure(&tasks, &id, "db", &config, ReadyCheck::Spawned).await.unwrap());
        let pid = tasks.status(&id).pid;
        assert!(pid.is_some());
        assert!(
            !ensure(&tasks, &id, "db", &config, ReadyCheck::Spawned).await.unwrap(),
            "second connect reuses it"
        );
        assert_eq!(tasks.status(&id).pid, pid);
        stop(&tasks, &id).await;
        assert!(!tasks.is_active(&id));
        assert_eq!(tasks.status(&id).state, TaskState::Stopped);
    }

    #[tokio::test]
    async fn a_changed_command_restarts_the_tunnel() {
        let tasks = manager();
        let (config, _) = long_running(None);
        let id = task_id("changed");
        ensure(&tasks, &id, "db", &config, ReadyCheck::Spawned).await.unwrap();
        let mut changed = config.clone();
        changed.args = changed.args.iter().map(|arg| arg.replace("30", "29")).collect();
        assert!(ensure(&tasks, &id, "db", &changed, ReadyCheck::Spawned).await.unwrap());
        assert_eq!(tasks.running_def(&id).unwrap().args, changed.args);
        stop(&tasks, &id).await;
    }

    #[tokio::test]
    async fn a_probe_reuses_the_connections_tunnel_or_runs_a_temporary_one() {
        let tasks = manager();
        let (config, _) = long_running(None);
        let own = task_id("probe");
        ensure(&tasks, &own, "db", &config, ReadyCheck::Spawned).await.unwrap();
        let probe = start_probe(&tasks, "probe", "db", &config, ReadyCheck::Spawned).await.unwrap();
        assert!(probe.temporary.is_none());
        probe.finish(&tasks).await;
        assert!(tasks.is_active(&own), "the connection's own tunnel keeps running");
        stop(&tasks, &own).await;

        let probe = start_probe(&tasks, "probe", "db", &config, ReadyCheck::Spawned).await.unwrap();
        let temporary = probe.temporary.clone().unwrap();
        assert!(tasks.is_active(&temporary));
        probe.finish(&tasks).await;
        assert!(!tasks.is_active(&temporary));
        assert!(tasks.logs(&temporary).is_empty(), "a temporary tunnel is forgotten");
    }

    #[tokio::test]
    async fn turned_off_tunnels_are_refused() {
        let tasks = Arc::new(TaskManager::new(tokio::runtime::Handle::current(), Arc::new(NoEvents), false));
        let (config, _) = long_running(None);
        let error = ensure(&tasks, &task_id("off"), "db", &config, ReadyCheck::Spawned)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), TUNNELS_DISABLED);
    }
}
