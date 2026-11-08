import { api, errorText } from "../core/api";
import { capabilities } from "../core/capabilities";
import { toast } from "../core/dialogs";
import { copyText } from "../core/dom";
import type { AppConfig, Diagnostics } from "../core/types";

/** Plain text for a bug report: versions, open connections and the newest errors (no secrets, no row data). */
export function diagnosticsText(d: Diagnostics): string {
  const lines = [
    `Fosta Studio ${d.appVersion} (${d.edition})`,
    `OS: ${d.os} ${d.arch}${d.osVersion ? ` · ${d.osVersion}` : ""}`,
    `Log folder: ${d.logFolder ?? "not set up"}${d.debugLogging ? " (debug logging on)" : ""}`,
    `Open connections: ${d.connections.length === 0 ? "none" : d.connections.map((c) => `${c.engine} ${c.version}${c.crashed ? " (crashed)" : ""}`).join(", ")}`,
  ];
  if (d.recentErrors.length > 0) {
    lines.push("Recent errors:");
    for (const error of d.recentErrors.slice(-20)) lines.push(`  ${error.time} ${error.level} ${error.code ?? ""} ${error.target}: ${error.message}`);
  }
  return lines.join("\n");
}

export async function copyDiagnostics(): Promise<void> {
  try {
    await copyText(diagnosticsText(await api.diagnostics()));
    toast("Diagnostics copied", "success");
  } catch (error) {
    toast(errorText(error), "error");
  }
}

/** Desktop: opens the folder. Web: the log lives on the server, so show where. */
export async function openLogFolder(): Promise<void> {
  try {
    if (capabilities().revealFile) {
      await api.openLogFolder();
    } else {
      const d = await api.diagnostics();
      toast(`The server writes its log to ${d.logFolder ?? "its data directory"} (inside the container).`, "info");
    }
  } catch (error) {
    toast(errorText(error), "error");
  }
}

export async function setDebugLogging(enabled: boolean, apply: (config: AppConfig) => void): Promise<void> {
  try {
    apply(await api.setDebugLogging(enabled));
    toast(enabled ? "Debug logging is on" : "Debug logging is off", "info");
  } catch (error) {
    toast(errorText(error), "error");
  }
}
