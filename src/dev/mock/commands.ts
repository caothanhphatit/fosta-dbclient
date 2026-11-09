/**
 * Workspace commands in the dev mock, like `src-tauri/src/tasks.rs` + `connection_command.rs`: a command "starts" in
 * about a second and logs kubectl-style lines, emitting `task-status` / `task-log` events. Connections (and "Test
 * connection") hold the command they use; one started for connections stops with its last holder. A program named
 * `false` exits at once (COMMAND_FAILED).
 */
import { emit } from "@tauri-apps/api/event";
import type { AppConfig, ConnectionProfile, TaskDef, TaskStatus } from "../../core/types";
import { localPort } from "../../features/tasks/commandForm";
import { mockError } from "./errors";
import { state } from "./state";

const START_DELAY_MS = 1000;

interface Runtime {
  status: TaskStatus;
  logs: string[];
  ready: Promise<void> | null;
}

const runtimes = new Map<string, Runtime>();
/** holder (connection id or probe) → command id */
const holders = new Map<string, string>();
let nextPid = 7000;
let nextProbe = 1;
let autoStarted = false;

function idle(id: string): TaskStatus {
  return { id, state: "stopped", pid: null, restarts: 0, message: null, forConnections: false };
}

function runtime(id: string): Runtime {
  let found = runtimes.get(id);
  if (!found) {
    found = { status: idle(id), logs: [], ready: null };
    runtimes.set(id, found);
  }
  return found;
}

function log(id: string, line: string): void {
  runtime(id).logs.push(line);
  void emit("task-log", { id, line });
}

function setStatus(id: string, change: Partial<TaskStatus>): void {
  const item = runtime(id);
  item.status = { ...item.status, ...change };
  void emit("task-status", item.status);
}

function allTasks(): TaskDef[] {
  return state.config.workspaces.flatMap((workspace) => workspace.tasks);
}

function taskOf(id: string): TaskDef {
  const task = allTasks().find((item) => item.id === id);
  if (!task) throw mockError("INVALID_INPUT", `Command '${id}' no longer exists`);
  return task;
}

const active = (id: string) => ["starting", "running", "restarting"].includes(runtimes.get(id)?.status.state ?? "stopped");

/** Starts `task`; resolves once it is running (rejects with COMMAND_FAILED when it exits first). */
function launch(task: TaskDef, forConnections: boolean, delay = START_DELAY_MS): Promise<void> {
  setStatus(task.id, { state: "starting", pid: nextPid++, restarts: 0, message: null, forConnections });
  log(task.id, `▶ ${[task.program, ...task.args].join(" ")}`);
  const ready = new Promise<void>((resolve, reject) => {
    setTimeout(() => {
      if (runtime(task.id).status.state !== "starting") return reject(failure(task, "exited before it was ready"));
      if (task.program === "false") {
        log(task.id, "error: unable to listen on any of the requested ports");
        log(task.id, "■ process exited (exit code: 1)");
        setStatus(task.id, { state: "failed", pid: null, forConnections: false, message: "process exited (exit code: 1)" });
        return reject(failure(task, "exited before it was ready"));
      }
      const local = localPort(task.args) ?? 13306;
      const remote = /^\d+:(\d+)$/.exec(task.args.find((arg) => /^\d+:\d+$/.test(arg)) ?? "")?.[1] ?? String(local);
      log(task.id, `Forwarding from 127.0.0.1:${local} -> ${remote}`);
      log(task.id, `Forwarding from [::1]:${local} -> ${remote}`);
      setStatus(task.id, { state: "running" });
      resolve();
    }, delay);
  });
  runtime(task.id).ready = ready;
  ready.catch(() => undefined);
  return ready;
}

function failure(task: TaskDef, what: string): unknown {
  const tail = runtime(task.id).logs.slice(-15).join("\n");
  return mockError("COMMAND_FAILED", `The command "${task.name}" ${what}. Last output:\n${tail}`);
}

function stop(id: string): void {
  if (!active(id)) return;
  log(id, "■ stopped");
  setStatus(id, { state: "stopped", pid: null, forConnections: false });
}

/** Starts the active workspace's auto-start commands (on the first status read, and on a workspace switch). */
export function startAutoCommands(): void {
  autoStarted = true;
  const workspace = state.config.workspaces.find((item) => item.id === state.config.activeWorkspace);
  for (const task of workspace?.tasks ?? []) {
    if (!task.autoStart) continue;
    if (active(task.id)) setStatus(task.id, { forConnections: false });
    else void launch(task, false, 0).catch(() => undefined);
  }
}

export function taskStatuses(): TaskStatus[] {
  if (!autoStarted) startAutoCommands();
  return allTasks().map((task) => runtimes.get(task.id)?.status ?? idle(task.id));
}

export function taskLogs(id: string): string[] {
  return runtimes.get(id)?.logs ?? [];
}

export function startTask(id: string): null {
  if (active(id)) throw mockError("INVALID_INPUT", `'${taskOf(id).name}' is already running`);
  void launch(taskOf(id), false).catch(() => undefined);
  return null;
}

export function stopTask(id: string): null {
  stop(id);
  return null;
}

export function restartTask(id: string): null {
  const forConnections = active(id) && Boolean(runtimes.get(id)?.status.forConnections);
  stop(id);
  void launch(taskOf(id), forConnections).catch(() => undefined);
  return null;
}

function withoutCommand(connections: ConnectionProfile[], id: string): ConnectionProfile[] {
  return connections.map((profile) => {
    if (profile.commandId !== id) return profile;
    const next = { ...profile };
    delete next.commandId;
    return next;
  });
}

/** Like `put_task`: a command that moves to another workspace leaves the connections of its old one. */
export function saveTask(task: TaskDef, workspaceId: string | null): AppConfig {
  const target = workspaceId ?? state.config.activeWorkspace;
  const workspaces = state.config.workspaces.map((workspace) => {
    if (workspace.id === target) {
      const index = workspace.tasks.findIndex((item) => item.id === task.id);
      const tasks = [...workspace.tasks];
      if (index >= 0) tasks[index] = task;
      else tasks.push(task);
      return { ...workspace, tasks };
    }
    if (!workspace.tasks.some((item) => item.id === task.id)) return workspace;
    return { ...workspace, tasks: workspace.tasks.filter((item) => item.id !== task.id), connections: withoutCommand(workspace.connections, task.id) };
  });
  state.config = { ...state.config, workspaces };
  return state.config;
}

/** Like `delete_task`: stops it, and the connections that used it no longer use a command. */
export function deleteTask(id: string): AppConfig {
  stop(id);
  const workspaces = state.config.workspaces.map((workspace) => ({
    ...workspace,
    tasks: workspace.tasks.filter((item) => item.id !== id),
    connections: withoutCommand(workspace.connections, id),
  }));
  state.config = { ...state.config, workspaces };
  return state.config;
}

function stopIfUnused(id: string): void {
  if ([...holders.values()].includes(id)) return;
  if (runtimes.get(id)?.status.forConnections) stop(id);
}

/** What `connect` does first: hold the command and wait until it runs (starting it for connections when it does not). */
export async function acquire(holder: string, profile: ConnectionProfile): Promise<void> {
  if (!profile.commandId) return release(holder);
  const task = taskOf(profile.commandId);
  const previous = holders.get(holder);
  holders.set(holder, task.id);
  if (previous && previous !== task.id) stopIfUnused(previous);
  const started = !active(task.id);
  try {
    if (started) await launch(task, true);
    else if (runtime(task.id).status.state !== "running") await runtime(task.id).ready;
  } catch (error) {
    holders.delete(holder);
    if (started) stopIfUnused(task.id);
    throw error;
  }
}

/** What `disconnect` / `delete_connection` do: let go; a command started for connections stops with its last holder. */
export function release(holder: string): void {
  const id = holders.get(holder);
  holders.delete(holder);
  if (id) stopIfUnused(id);
}

/** What `save_connection` does: another command (or none) lets go of the old one. */
export function connectionSaved(profile: ConnectionProfile): void {
  const held = holders.get(profile.id);
  if (held && held !== profile.commandId) release(profile.id);
}

/** What `test_connection` does: the running command, or one started for the test and stopped after it. */
export async function withProbe<T>(profile: ConnectionProfile, probe: () => T): Promise<T> {
  if (!profile.commandId) return probe();
  const holder = `probe:${nextProbe++}`;
  await acquire(holder, profile);
  try {
    return probe();
  } finally {
    release(holder);
  }
}
