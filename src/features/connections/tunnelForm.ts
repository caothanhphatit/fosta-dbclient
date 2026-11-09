/**
 * The connection's tunnel command (a program started before connecting, e.g. `kubectl port-forward`), as the
 * connection dialog edits it: pure helpers, tested in `tunnelForm.test.ts`. The section itself is `tunnelSection.ts`.
 */
import { formatArgs, parseArgs } from "../../core/shell";
import type { TaskDef, TunnelConfig } from "../../core/types";

export const DEFAULT_TUNNEL_TIMEOUT = 30;

/** What the section's inputs hold. */
export interface TunnelValues {
  program: string;
  args: string;
  readyPattern: string;
  timeout: string;
}

/** The task id the backend runs a connection's tunnel under (`task-status` / `task-log` events carry it). */
export function tunnelTaskId(connectionId: string): string {
  return `tunnel:${connectionId}`;
}

export function isTunnelTaskId(id: string): boolean {
  return id.startsWith("tunnel:") || id.startsWith("tunnel-probe:");
}

export function tunnelValues(tunnel: TunnelConfig | null | undefined): TunnelValues {
  return {
    program: tunnel?.program ?? "",
    args: formatArgs(tunnel?.args ?? []),
    readyPattern: tunnel?.readyPattern ?? "",
    timeout: String(tunnel?.readyTimeoutSecs ?? DEFAULT_TUNNEL_TIMEOUT),
  };
}

/** The config for the inputs; null when no program is set. */
export function tunnelFromValues(values: TunnelValues): TunnelConfig | null {
  const program = values.program.trim();
  if (!program) return null;
  const timeout = Number(values.timeout);
  return {
    program,
    args: parseArgs(values.args),
    readyPattern: values.readyPattern.trim() || null,
    readyTimeoutSecs: Number.isInteger(timeout) && timeout > 0 ? Math.min(timeout, 600) : DEFAULT_TUNNEL_TIMEOUT,
  };
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

/** `kubectl port-forward` to the engine's service, on its default port + 10000 locally (e.g. 13306:3306). */
export function portForwardTemplate(engineId: string, defaultPort: number | null | undefined): TunnelValues {
  const remote = defaultPort && defaultPort > 0 ? defaultPort : 3306;
  const local = remote + 10000 < 65536 ? remote + 10000 : remote + 1;
  return {
    program: "kubectl",
    args: `port-forward -n default svc/${engineId} ${local}:${remote}`,
    readyPattern: "Forwarding from",
    timeout: String(DEFAULT_TUNNEL_TIMEOUT),
  };
}

/** Where the connection should point once the template is filled: 127.0.0.1 and the local port, unless a remote host is set. */
export function endpointForTemplate(host: string, values: TunnelValues): { host: string; port: number } | null {
  const current = host.trim();
  if (current && current !== "127.0.0.1" && current !== "localhost") return null;
  const port = localPort(parseArgs(values.args));
  return port ? { host: "127.0.0.1", port } : null;
}

/** The tunnel as a task, for the log tab and the assistant. */
export function tunnelTask(connectionId: string, name: string, tunnel: TunnelConfig): TaskDef {
  return {
    id: tunnelTaskId(connectionId),
    name: `Tunnel · ${name}`,
    program: tunnel.program,
    args: tunnel.args,
    autoStart: false,
    autoRestart: true,
    readyPattern: tunnel.readyPattern ?? null,
  };
}
