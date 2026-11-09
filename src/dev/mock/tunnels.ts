/**
 * Tunnel commands in the dev mock: a tunnel "starts" in about a second and logs a kubectl-style line, emitting the
 * same `task-status` / `task-log` events as the backend. A program named `false` exits at once (TUNNEL_FAILED).
 */
import { emit } from "@tauri-apps/api/event";
import type { ConnectionProfile, TaskStatus, TunnelConfig } from "../../core/types";
import { localPort, tunnelTaskId } from "../../features/connections/tunnelForm";
import { mockError } from "./errors";

const START_DELAY_MS = 1000;

interface MockTunnel {
  command: string;
  status: TaskStatus;
  logs: string[];
}

const tunnels = new Map<string, MockTunnel>();
let nextPid = 7000;
let nextProbe = 1;

function idle(id: string): TaskStatus {
  return { id, state: "stopped", pid: null, restarts: 0, message: null };
}

function commandOf(tunnel: TunnelConfig): string {
  return [tunnel.program, ...tunnel.args].join(" ");
}

function log(id: string, line: string): void {
  const tunnel = tunnels.get(id);
  if (!tunnel) return;
  tunnel.logs.push(line);
  void emit("task-log", { id, line });
}

function setStatus(id: string, change: Partial<TaskStatus>): void {
  const tunnel = tunnels.get(id);
  if (!tunnel) return;
  tunnel.status = { ...tunnel.status, ...change };
  void emit("task-status", tunnel.status);
}

export function tunnelStatus(connectionId: string): TaskStatus {
  const id = tunnelTaskId(connectionId);
  return tunnels.get(id)?.status ?? idle(id);
}

export function tunnelLogs(connectionId: string): string[] {
  return tunnels.get(tunnelTaskId(connectionId))?.logs ?? [];
}

/** Starts the tunnel `id` (or reuses it when it runs the same command) and resolves once it is ready. */
async function ensure(id: string, tunnel: TunnelConfig): Promise<void> {
  const running = tunnels.get(id);
  if (running?.status.state === "running" && running.command === commandOf(tunnel)) return;
  const logs = running?.logs ?? [];
  tunnels.set(id, { command: commandOf(tunnel), status: { ...idle(id), state: "starting", pid: nextPid++ }, logs });
  setStatus(id, {});
  log(id, `▶ ${commandOf(tunnel)}`);
  await new Promise((resolve) => setTimeout(resolve, START_DELAY_MS));
  if (tunnel.program === "false") {
    log(id, "error: unable to listen on any of the requested ports");
    log(id, "■ process exited (exit code: 1)");
    setStatus(id, { state: "stopped", pid: null });
    const tail = tunnels.get(id)?.logs.slice(-15).join("\n") ?? "";
    throw mockError("TUNNEL_FAILED", `The tunnel command exited before it was ready. Last output:\n${tail}`);
  }
  const local = localPort(tunnel.args) ?? 13306;
  const remote = /^\d+:(\d+)$/.exec(tunnel.args.find((arg) => /^\d+:\d+$/.test(arg)) ?? "")?.[1] ?? String(local);
  log(id, `Forwarding from 127.0.0.1:${local} -> ${remote}`);
  log(id, `Forwarding from [::1]:${local} -> ${remote}`);
  setStatus(id, { state: "running" });
}

export function stopTunnel(id: string): void {
  const tunnel = tunnels.get(id);
  if (!tunnel || tunnel.status.state === "stopped") return;
  log(id, "■ stopped");
  setStatus(id, { state: "stopped", pid: null });
}

/** What `connect` does first for a profile with a tunnel. */
export function startTunnel(profile: ConnectionProfile): Promise<void> {
  return profile.tunnel ? ensure(tunnelTaskId(profile.id), profile.tunnel) : Promise.resolve();
}

/** What `test_connection` does first: the connection's running tunnel, or a temporary one stopped afterwards. */
export async function withProbeTunnel<T>(profile: ConnectionProfile, probe: () => T): Promise<T> {
  if (!profile.tunnel) return probe();
  const own = tunnels.get(tunnelTaskId(profile.id));
  if (own?.status.state === "running" && own.command === commandOf(profile.tunnel)) return probe();
  const id = `tunnel-probe:${nextProbe++}`;
  try {
    await ensure(id, profile.tunnel);
    return probe();
  } finally {
    stopTunnel(id);
    tunnels.delete(id);
  }
}

/** Saving a profile whose command changed stops the running tunnel. */
export function tunnelSaved(before: ConnectionProfile | undefined, after: ConnectionProfile): void {
  const command = (profile: ConnectionProfile | undefined) => (profile?.tunnel?.program.trim() ? commandOf(profile.tunnel) : "");
  if (before && command(before) !== command(after)) stopTunnel(tunnelTaskId(after.id));
}
