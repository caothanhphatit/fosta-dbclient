/**
 * Updates in the dev mock: `check_update` always finds 0.1.1, so the status bar item and its dialog can be previewed.
 * `__fostaMock.setInstallKind("portable" | "web")` changes the edition the dialog is for (then Settings → Check for
 * updates now); the Tauri updater and process plugins are simulated (a 2-second download, "Restart" reloads the page).
 * Open the page with `?whatsNew` to see the "What's new" note, or with `?installKind=portable` (or `web`) to start as that edition.
 */
import type { InstallKind, UpdateInfo } from "../../core/types";

const LATEST = "0.1.1";
let installKind = (new URLSearchParams(window.location.search).get("installKind") ?? "installer") as InstallKind;

export function setInstallKind(kind: InstallKind): void {
  installKind = kind;
}

export function mockInstallKind(): InstallKind {
  return installKind;
}

export function mockCheckUpdate(): UpdateInfo {
  return {
    current: __APP_VERSION__,
    latest: LATEST,
    newer: true,
    url: `https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v${LATEST}`,
    publishedAt: new Date().toISOString(),
  };
}

export function mockWhatsNew(): string | null {
  return new URLSearchParams(window.location.search).has("whatsNew") ? LATEST : null;
}

/** `plugin:updater|check`: the metadata the plugin turns into an `Update` resource. */
export function mockUpdaterCheck(): unknown {
  return { rid: 9001, currentVersion: __APP_VERSION__, version: LATEST, date: new Date().toISOString(), body: "", rawJson: {} };
}

/** `plugin:updater|download`: `Started`, ten `Progress` chunks over two seconds, `Finished`; returns the bytes' resource id. */
export async function mockUpdaterDownload(emit: (event: unknown) => void): Promise<number> {
  const total = 9_400_000;
  emit({ event: "Started", data: { contentLength: total } });
  for (let chunk = 0; chunk < 10; chunk++) {
    await new Promise((resolve) => setTimeout(resolve, 200));
    emit({ event: "Progress", data: { chunkLength: total / 10 } });
  }
  emit({ event: "Finished" });
  return 9002;
}

/** `plugin:updater|install` and `plugin:process|restart`: the page reloads as the app would restart. */
export function mockRestart(restart = true): null {
  console.info(`devMock: update ${LATEST} installed${restart ? ", restarting" : " (on exit)"}`);
  if (restart) setTimeout(() => window.location.reload(), 300);
  return null;
}
