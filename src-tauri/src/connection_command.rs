//! The workspace command a connection uses (`ConnectionProfile::command`, typically `kubectl port-forward`). Connecting
//! reuses the command when it runs, however it was started, and otherwise starts it "for connections" and waits until
//! it is ready. Connections (and "Test connection" probes) are its holders in the [`TaskManager`]: when the last one
//! lets go, a command started for connections stops; one started with the workspace or by hand keeps running.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tokio::net::TcpStream;

use crate::{
    error::{AppError, AppResult},
    tasks::{ReadyCheck, TaskDef, TaskManager, TaskState, TASKS_DISABLED},
};

/// The error code of a command that did not become ready for a connection.
pub const COMMAND_FAILED: &str = "COMMAND_FAILED";
/// The error code of a connection whose command is not running, for callers that may not start it (MCP clients).
pub const COMMAND_NOT_RUNNING: &str = "COMMAND_NOT_RUNNING";

const LOG_TAIL: usize = 15;
const READY_POLL: Duration = Duration::from_millis(100);
const PORT_ATTEMPT: Duration = Duration::from_secs(1);

static NEXT_PROBE: AtomicU64 = AtomicU64::new(1);

/// When a command without a ready pattern is ready for a connection: when the connection's endpoint accepts TCP
/// connections (127.0.0.1 when the host is empty).
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

/// Makes `holder` use the command `def` and waits until it is ready, up to the command's timeout. A running command
/// is reused (whoever started it); otherwise it is started for connections. Returns whether this call started it.
/// On failure `holder` lets go of it, and a command this call started is stopped again unless someone else uses it.
pub async fn acquire(tasks: &Arc<TaskManager>, holder: &str, def: &TaskDef, ready: ReadyCheck) -> AppResult<bool> {
    if !tasks.enabled() {
        return Err(AppError::new(TASKS_DISABLED));
    }
    if let Some(previous) = tasks.hold(holder, &def.id) {
        stop_if_unused(tasks, &previous).await;
    }
    let started = match tasks.start_for_connections(def.clone(), ready.clone()) {
        Ok(started) => started,
        Err(error) => {
            tasks.unhold(holder);
            return Err(error);
        }
    };
    let deadline = Instant::now() + def.ready_timeout();
    let mut result = wait_running(tasks, def, deadline, started).await;
    if result.is_ok() && !started {
        // A command started with the workspace or by hand counts as running once spawned: without a ready pattern,
        // wait for the connection's port too.
        let running = tasks.running_def(&def.id).unwrap_or_else(|| def.clone());
        if let (None, ReadyCheck::Port { host, port }) = (pattern(&running), &ready) {
            result = wait_port(tasks, def, host, *port, deadline).await;
        }
    }
    if let Err(error) = result {
        tasks.unhold(holder);
        if started {
            stop_if_unused(tasks, &def.id).await;
        }
        return Err(error);
    }
    Ok(started)
}

/// Like [`acquire`], but only reuses a command that is already running and never starts one: when `def` is not
/// running the error is [`COMMAND_NOT_RUNNING`]. MCP clients connect through this.
pub async fn acquire_running(tasks: &TaskManager, holder: &str, def: &TaskDef, ready: ReadyCheck) -> AppResult<()> {
    if !tasks.enabled() {
        return Err(AppError::new(TASKS_DISABLED));
    }
    if !tasks.is_active(&def.id) {
        return Err(AppError::coded(
            COMMAND_NOT_RUNNING,
            format!("The command \"{}\" this connection needs is not running.", def.name),
        ));
    }
    if let Some(previous) = tasks.hold(holder, &def.id) {
        stop_if_unused(tasks, &previous).await;
    }
    let deadline = Instant::now() + def.ready_timeout();
    let mut result = wait_running(tasks, def, deadline, false).await;
    if result.is_ok() {
        let running = tasks.running_def(&def.id).unwrap_or_else(|| def.clone());
        if let (None, ReadyCheck::Port { host, port }) = (pattern(&running), &ready) {
            result = wait_port(tasks, def, host, *port, deadline).await;
        }
    }
    if let Err(error) = result {
        tasks.unhold(holder);
        return Err(error);
    }
    Ok(())
}

/// Lets go of the command `holder` uses; it stops when it was started for connections and nobody else uses it.
pub async fn release(tasks: &TaskManager, holder: &str) {
    if let Some(id) = tasks.unhold(holder) {
        stop_if_unused(tasks, &id).await;
    }
}

/// [`release`] without waiting for the process to end.
pub fn release_now(tasks: &TaskManager, holder: &str) {
    if let Some(id) = tasks.unhold(holder) {
        tasks.stop_if_unused(&id);
    }
}

async fn stop_if_unused(tasks: &TaskManager, id: &str) {
    if tasks.stop_if_unused(id) {
        let _ = tasks.wait_until_stopped(id).await;
    }
}

fn pattern(def: &TaskDef) -> Option<&str> {
    def.ready_pattern.as_deref().filter(|pattern| !pattern.is_empty())
}

/// Waits until the command is running. `fresh`: it was just started, so any exit fails (rather than waiting for the
/// restart).
async fn wait_running(tasks: &TaskManager, def: &TaskDef, deadline: Instant, fresh: bool) -> AppResult<()> {
    loop {
        let status = tasks.status(&def.id);
        match status.state {
            TaskState::Running if !(fresh && status.restarts > 0) => return Ok(()),
            TaskState::Failed | TaskState::Stopped => return Err(failure(tasks, def, "exited before it was ready")),
            TaskState::Restarting if fresh => return Err(failure(tasks, def, "exited before it was ready")),
            _ if fresh && status.restarts > 0 => return Err(failure(tasks, def, "exited before it was ready")),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(not_ready(tasks, def));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Waits until `host:port` accepts a TCP connection.
async fn wait_port(tasks: &TaskManager, def: &TaskDef, host: &str, port: u16, deadline: Instant) -> AppResult<()> {
    loop {
        let attempt = tokio::time::timeout(PORT_ATTEMPT, TcpStream::connect((host, port))).await;
        if matches!(attempt, Ok(Ok(_))) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(not_ready(tasks, def));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

fn not_ready(tasks: &TaskManager, def: &TaskDef) -> AppError {
    failure(tasks, def, &format!("was not ready after {}s", def.ready_timeout().as_secs()))
}

fn failure(tasks: &TaskManager, def: &TaskDef, what: &str) -> AppError {
    let logs = tasks.logs(&def.id);
    let tail = logs[logs.len().saturating_sub(LOG_TAIL)..].join("\n");
    let message = if tail.is_empty() {
        format!("The command \"{}\" {what}.", def.name)
    } else {
        format!("The command \"{}\" {what}. Last output:\n{tail}", def.name)
    };
    AppError::coded(COMMAND_FAILED, message)
}

/// The command a "Test connection" uses; [`Probe::finish`] lets go of it.
pub struct Probe {
    holder: String,
}

/// Readies the command for a probe: the running one, or one started for the probe and stopped by [`Probe::finish`]
/// unless a connection uses it by then.
pub async fn probe(tasks: &Arc<TaskManager>, def: &TaskDef, ready: ReadyCheck) -> AppResult<Probe> {
    let holder = format!("probe:{}", NEXT_PROBE.fetch_add(1, Ordering::Relaxed));
    acquire(tasks, &holder, def, ready).await?;
    Ok(Probe { holder })
}

impl Probe {
    pub async fn finish(self, tasks: &TaskManager) {
        release(tasks, &self.holder).await;
    }
}

/// Small cross-platform commands for tests (Windows and Linux CI).
#[cfg(test)]
pub(crate) mod testing {
    use crate::tasks::TaskDef;

    pub(crate) fn command(id: &str, program: &str, args: &[&str], pattern: Option<&str>, timeout: u64) -> TaskDef {
        TaskDef {
            id: id.into(),
            name: "Port-forward".into(),
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            auto_start: false,
            auto_restart: true,
            ready_pattern: pattern.map(str::to_owned),
            ready_timeout_secs: timeout,
        }
    }

    /// A process that prints one line and keeps running (no grandchild, so stopping it ends it), and that line.
    pub(crate) fn long_running(id: &str) -> (TaskDef, &'static str) {
        if cfg!(windows) {
            // ping's text is localized; the address is in every language.
            (command(id, "ping", &["-n", "30", "127.0.0.1"], None, 10), "127.0.0.1")
        } else {
            (
                command(id, "sh", &["-c", "echo Forwarding from 127.0.0.1; exec sleep 30"], None, 10),
                "Forwarding from",
            )
        }
    }

    /// A process that prints "boom: port is in use" and exits with code 3.
    pub(crate) fn exits_early(id: &str) -> TaskDef {
        if cfg!(windows) {
            command(id, "cmd", &["/C", "echo boom: port is in use& exit /b 3"], Some("never"), 10)
        } else {
            command(id, "sh", &["-c", "echo boom: port is in use; exit 3"], Some("never"), 10)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{command, exits_early, long_running};
    use super::*;
    use crate::events::NoEvents;
    use std::net::TcpListener;

    fn manager() -> Arc<TaskManager> {
        Arc::new(TaskManager::new(tokio::runtime::Handle::current(), Arc::new(NoEvents), true))
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn code(error: &AppError) -> Option<&str> {
        error.field("code").and_then(|code| code.as_str())
    }

    async fn stop(tasks: &TaskManager, id: &str) {
        tasks.stop(id);
        tasks.wait_until_stopped(id).await.unwrap();
    }

    #[test]
    fn the_timeout_defaults_to_30_seconds() {
        // 0.1.0 files have no readyTimeoutSecs.
        let parsed: TaskDef = serde_json::from_str(r#"{"id":"t","name":"pf","program":"kubectl"}"#).unwrap();
        assert_eq!(parsed.ready_timeout_secs, 30);
        assert!(parsed.auto_restart);
        assert_eq!(command("t", "x", &[], None, 0).ready_timeout(), Duration::from_secs(30));
        assert_eq!(command("t", "x", &[], None, 9999).ready_timeout(), Duration::from_secs(600));
        assert!(matches!(ready_check(" ", 5432), ReadyCheck::Port { host, port: 5432 } if host == "127.0.0.1"));
        assert!(matches!(ready_check("db", 0), ReadyCheck::Spawned));
    }

    #[tokio::test]
    async fn ready_when_the_output_contains_the_pattern() {
        let tasks = manager();
        let (mut def, line) = long_running("pattern");
        def.ready_pattern = Some(line.to_owned());
        assert!(acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap());
        assert_eq!(tasks.status(&def.id).state, TaskState::Running);
        assert!(tasks.status(&def.id).for_connections);
        assert!(tasks.logs(&def.id).iter().any(|logged| logged.contains(line)));
        release(&tasks, "a").await;
        assert!(!tasks.is_active(&def.id));
    }

    #[tokio::test]
    async fn ready_when_the_port_accepts_connections() {
        let tasks = manager();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (def, _) = long_running("port");
        assert!(acquire(&tasks, "a", &def, ready_check("", port)).await.unwrap());
        assert_eq!(tasks.status(&def.id).state, TaskState::Running);
        release(&tasks, "a").await;
    }

    #[tokio::test]
    async fn a_closed_port_times_out_and_stops_the_command() {
        let tasks = manager();
        let (mut def, line) = long_running("closed");
        def.ready_timeout_secs = 1;
        let error = acquire(&tasks, "a", &def, ready_check("127.0.0.1", free_port())).await.unwrap_err();
        assert_eq!(code(&error), Some(COMMAND_FAILED));
        assert!(error.to_string().contains("\"Port-forward\" was not ready after 1s"), "{error}");
        assert!(error.to_string().contains(line), "{error}");
        assert!(!tasks.is_active(&def.id));
        assert_eq!(tasks.held_by("a"), None);
    }

    #[tokio::test]
    async fn an_early_exit_fails_with_the_log_tail() {
        let tasks = manager();
        let def = exits_early("early");
        let error = acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap_err();
        assert_eq!(code(&error), Some(COMMAND_FAILED));
        let message = error.to_string();
        assert!(message.contains("exited before it was ready"), "{message}");
        assert!(message.contains("boom: port is in use"), "{message}");
        assert!(!tasks.is_active(&def.id), "a failed command is not restarted in the background");
    }

    #[tokio::test]
    async fn a_missing_program_fails() {
        let tasks = manager();
        let def = command("missing", "fosta-no-such-program", &[], None, 5);
        let error = acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap_err();
        assert!(error.to_string().contains("cannot start 'fosta-no-such-program'"), "{error}");
    }

    #[tokio::test]
    async fn two_connections_share_a_command_until_the_last_one_lets_go() {
        let tasks = manager();
        let (def, _) = long_running("shared");
        assert!(acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap());
        let pid = tasks.status(&def.id).pid;
        assert!(pid.is_some());
        assert!(!acquire(&tasks, "b", &def, ReadyCheck::Spawned).await.unwrap(), "b reuses it");
        assert!(
            !acquire(&tasks, "b", &def, ReadyCheck::Spawned).await.unwrap(),
            "connecting again holds it once"
        );
        assert_eq!(tasks.status(&def.id).pid, pid);
        assert_eq!(tasks.holder_count(&def.id), 2);
        release(&tasks, "a").await;
        assert!(tasks.is_active(&def.id), "b still uses it");
        release(&tasks, "a").await;
        assert!(tasks.is_active(&def.id), "releasing twice changes nothing");
        release(&tasks, "b").await;
        assert!(!tasks.is_active(&def.id));
        assert_eq!(tasks.status(&def.id).state, TaskState::Stopped);
        assert!(!tasks.status(&def.id).for_connections);
    }

    #[tokio::test]
    async fn a_command_started_with_the_workspace_or_by_hand_keeps_running() {
        let tasks = manager();
        let (def, _) = long_running("manual");
        tasks.start(def.clone()).unwrap();
        assert!(!acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap());
        assert!(!tasks.status(&def.id).for_connections);
        release(&tasks, "a").await;
        assert!(tasks.is_active(&def.id), "started by hand: it keeps running");
        stop(&tasks, &def.id).await;

        // Started for a connection, then its workspace opens with it as an auto-start command: it now belongs there.
        let auto = TaskDef {
            auto_start: true,
            ..def.clone()
        };
        assert!(acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap());
        tasks.start_auto(std::slice::from_ref(&auto));
        assert!(!tasks.status(&def.id).for_connections);
        release(&tasks, "a").await;
        assert!(tasks.is_active(&def.id));
        stop(&tasks, &def.id).await;
    }

    #[tokio::test]
    async fn a_restart_keeps_a_command_started_for_connections() {
        let tasks = manager();
        let (def, _) = long_running("restart");
        acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap();
        tasks.restart(def.clone()).await.unwrap();
        assert!(tasks.status(&def.id).for_connections);
        release(&tasks, "a").await;
        assert!(!tasks.is_active(&def.id));
    }

    #[tokio::test]
    async fn reusing_a_command_without_a_pattern_waits_for_the_port() {
        let tasks = manager();
        let (mut def, _) = long_running("reuse-port");
        def.ready_timeout_secs = 1;
        tasks.start(def.clone()).unwrap();
        let error = acquire(&tasks, "a", &def, ready_check("127.0.0.1", free_port())).await.unwrap_err();
        assert_eq!(code(&error), Some(COMMAND_FAILED));
        assert!(tasks.is_active(&def.id), "not started by this connect, so not stopped");
        assert_eq!(tasks.held_by("a"), None);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!acquire(&tasks, "a", &def, ready_check("127.0.0.1", port)).await.unwrap());
        stop(&tasks, &def.id).await;
    }

    #[tokio::test]
    async fn switching_commands_lets_go_of_the_previous_one() {
        let tasks = manager();
        let (first, _) = long_running("first");
        let (second, _) = long_running("second");
        acquire(&tasks, "a", &first, ReadyCheck::Spawned).await.unwrap();
        acquire(&tasks, "a", &second, ReadyCheck::Spawned).await.unwrap();
        assert!(!tasks.is_active(&first.id));
        assert!(tasks.is_active(&second.id));
        release_now(&tasks, "a");
        tasks.wait_until_stopped(&second.id).await.unwrap();
    }

    #[tokio::test]
    async fn a_probe_reuses_a_running_command_or_runs_it_for_the_test() {
        let tasks = manager();
        let (def, _) = long_running("probe");
        let test = probe(&tasks, &def, ReadyCheck::Spawned).await.unwrap();
        assert!(tasks.is_active(&def.id));
        test.finish(&tasks).await;
        assert!(!tasks.is_active(&def.id), "started for the test: stopped after it");

        acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap();
        probe(&tasks, &def, ReadyCheck::Spawned).await.unwrap().finish(&tasks).await;
        assert!(tasks.is_active(&def.id), "the connection's command keeps running");
        release(&tasks, "a").await;

        let test = probe(&tasks, &def, ReadyCheck::Spawned).await.unwrap();
        acquire(&tasks, "b", &def, ReadyCheck::Spawned).await.unwrap();
        test.finish(&tasks).await;
        assert!(tasks.is_active(&def.id), "a connection started using it meanwhile");
        release(&tasks, "b").await;
        assert!(!tasks.is_active(&def.id));
    }

    #[tokio::test]
    async fn acquire_running_reuses_a_running_command_and_never_starts_one() {
        let tasks = manager();
        let (def, _) = long_running("reuse-only");
        let error = acquire_running(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap_err();
        assert_eq!(code(&error), Some(COMMAND_NOT_RUNNING));
        assert!(!tasks.is_active(&def.id), "not started");
        assert_eq!(tasks.held_by("a"), None);

        tasks.start(def.clone()).unwrap();
        acquire_running(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap();
        assert_eq!(tasks.held_by("a").as_deref(), Some(def.id.as_str()));
        release(&tasks, "a").await;
        assert!(tasks.is_active(&def.id), "started by hand: it keeps running");
        stop(&tasks, &def.id).await;
    }

    #[tokio::test]
    async fn turned_off_commands_are_refused() {
        let tasks = Arc::new(TaskManager::new(tokio::runtime::Handle::current(), Arc::new(NoEvents), false));
        let (def, _) = long_running("off");
        let error = acquire(&tasks, "a", &def, ReadyCheck::Spawned).await.unwrap_err();
        assert_eq!(error.to_string(), TASKS_DISABLED);
        assert_eq!(tasks.held_by("a"), None);
    }
}
