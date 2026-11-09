/**
 * Pure helpers of the command dialog (`taskDialog.ts`) and of the connection dialog's command picker: the kubectl
 * port-forward template, its local port, and which connections use a command. Tested in `commandForm.test.ts`.
 */
import { formatArgs, parseArgs } from "../../core/shell";
import type { ConnectionProfile, TaskDef } from "../../core/types";

export const DEFAULT_READY_TIMEOUT = 30;
export const MAX_READY_TIMEOUT = 600;

/** What the dialog's inputs hold. */
export interface CommandValues {
  name: string;
  program: string;
  args: string;
  readyPattern: string;
  timeout: string;
}

export function commandValues(task: TaskDef | null): CommandValues {
  return {
    name: task?.name ?? "",
    program: task?.program ?? "kubectl",
    args: formatArgs(task?.args ?? []),
    readyPattern: task?.readyPattern ?? "",
    timeout: String(task?.readyTimeoutSecs ?? DEFAULT_READY_TIMEOUT),
  };
}

/** The timeout typed in the dialog, or the default when it is not a whole number of seconds. */
export function readyTimeout(text: string): number {
  const timeout = Number(text);
  return Number.isInteger(timeout) && timeout > 0 ? Math.min(timeout, MAX_READY_TIMEOUT) : DEFAULT_READY_TIMEOUT;
}

/** The local port a port-forward listens on: `13306` in `13306:3306`, or a lone port (`port-forward svc/x 5432`). */
export function localPort(args: string[]): number | null {
  const valid = (text: string) => {
    const port = Number(text);
    return Number.isInteger(port) && port > 0 && port < 65536 ? port : null;
  };
  for (const arg of args) {
    const pair = /^(\d+):\d+$/.exec(arg);
    if (pair) return valid(pair[1]);
  }
  const lone = args.length > 2 && args[0] === "port-forward" ? args[args.length - 1] : "";
  return /^\d+$/.test(lone) ? valid(lone) : null;
}

/** What a template is filled for: the connection's engine, its default port, and where the connection points now. */
export interface TemplateTarget {
  engineId: string;
  engineLabel?: string;
  defaultPort: number | null | undefined;
  host?: string;
  port?: number | null;
}

function isLocal(host: string | undefined): boolean {
  const current = (host ?? "").trim();
  return !current || current === "127.0.0.1" || current === "localhost";
}

/**
 * `kubectl port-forward` to the engine's service. Locally it listens on the connection's port when the connection
 * points at this machine on a port other than the engine's default (it already expects a forward there), else on the
 * default port + 10000 (e.g. 13306:3306).
 */
export function portForwardTemplate(target: TemplateTarget): CommandValues {
  const remote = target.defaultPort && target.defaultPort > 0 ? target.defaultPort : 3306;
  const own = target.port && target.port > 0 && target.port < 65536 && target.port !== remote && isLocal(target.host) ? target.port : null;
  const local = own ?? (remote + 10000 < 65536 ? remote + 10000 : remote + 1);
  return {
    name: `${target.engineLabel ?? target.engineId} port-forward`,
    program: "kubectl",
    args: `port-forward -n default svc/${target.engineId} ${local}:${remote}`,
    readyPattern: "Forwarding from",
    timeout: String(DEFAULT_READY_TIMEOUT),
  };
}

/** Where a connection should point to use `task`: 127.0.0.1 and its local port, unless a remote host is set. */
export function endpointForCommand(host: string, task: Pick<TaskDef, "args">): { host: string; port: number } | null {
  if (!isLocal(host)) return null;
  const port = localPort(task.args);
  return port ? { host: "127.0.0.1", port } : null;
}

/** The connections that use the command `id`. */
export function commandUsers(id: string, connections: ConnectionProfile[]): ConnectionProfile[] {
  return connections.filter((profile) => profile.commandId === id);
}

/** The task the dialog's inputs describe (throws without a program). */
export function taskFromValues(id: string, values: CommandValues, autoStart: boolean, autoRestart: boolean): TaskDef {
  const program = values.program.trim();
  if (!program) throw new Error("Program is required");
  return {
    id,
    name: values.name.trim() || program,
    program,
    args: parseArgs(values.args),
    autoStart,
    autoRestart,
    readyPattern: values.readyPattern.trim() || null,
    readyTimeoutSecs: readyTimeout(values.timeout),
  };
}
