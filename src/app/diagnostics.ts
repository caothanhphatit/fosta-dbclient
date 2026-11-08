import { showError } from "../core/errorPanel";
import { api } from "../core/api";
import { capabilities } from "../core/capabilities";
import { toast } from "../core/dialogs";
import { copyText } from "../core/dom";
import { diagnosticsText } from "../core/errors";
import type { AppConfig } from "../core/types";

export { diagnosticsText };

export async function copyDiagnostics(): Promise<void> {
  try {
    await copyText(diagnosticsText(await api.diagnostics()));
    toast("Diagnostics copied", "success");
  } catch (error) {
    showError(error, "Copy diagnostics");
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
    showError(error, "Open the log folder");
  }
}

export async function setDebugLogging(enabled: boolean, apply: (config: AppConfig) => void): Promise<void> {
  try {
    apply(await api.setDebugLogging(enabled));
    toast(enabled ? "Debug logging is on" : "Debug logging is off", "info");
  } catch (error) {
    showError(error, "Change debug logging");
  }
}
