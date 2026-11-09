/**
 * Updates: the status bar item, its dialog and the "What's new" note.
 *
 * - Every edition asks the backend (`check_update`) at startup, at most once a day, and on Settings → Check for updates now.
 * - The installed desktop app (`install_kind` "installer") then downloads the update in the background with the Tauri
 *   updater and installs it when the app quits; "Restart to update" does it now. If the updater fails, it falls back to
 *   the release link.
 * - The portable app links to the release page; the web edition shows the Docker commands.
 */
import type { Update } from "@tauri-apps/plugin-updater";
import { api } from "../core/api";
import { capabilities } from "../core/capabilities";
import { openModal, toast } from "../core/dialogs";
import { copyText, h, icon, iconButton } from "../core/dom";
import { showError } from "../core/errorPanel";
import { errorCode } from "../core/errors";
import { isTauri } from "../core/transport";
import type { InstallKind, UpdateInfo } from "../core/types";
import { dockerUpdateCommands, downloadPercent, releaseUrl, updateLabel, type UpdatePhase } from "./updateLogic";

/** What the update item needs from the shell. */
export interface UpdateHost {
  /** Asks first when open transactions or unsaved edits would be lost; true to go ahead. */
  confirmRestart(): Promise<boolean>;
}

let host: UpdateHost | null = null;
let info: UpdateInfo | null = null;
let kind: InstallKind = "portable";
let phase: UpdatePhase = "none";
let percent: number | null = null;
let pending: Update | null = null;
let busy = false;
/** Redraws the open dialog while the download progresses. */
let refreshDialog: (() => void) | null = null;

const item = h("button", { class: "status-update", type: "button", hidden: true });
item.addEventListener("click", () => openUpdateDialog());

/** The status bar item; hidden until a newer version is found. */
export function updateStatusItem(): HTMLElement {
  return item;
}

function render(): void {
  const version = pending?.version ?? info?.latest ?? "";
  item.hidden = phase === "none";
  item.replaceChildren(icon("download"), h("span", {}, updateLabel(phase, version, percent)));
  item.title = phase === "ready" ? `Fosta Studio ${version} is ready; it installs when you quit` : `Fosta Studio ${version} is available`;
  item.classList.toggle("ready", phase === "ready");
  refreshDialog?.();
}

/** At startup: the "What's new" note once after an update, then the daily check. */
export async function startUpdates(shell: UpdateHost): Promise<void> {
  host = shell;
  try {
    const version = await api.whatsNew();
    if (version) showWhatsNew(version);
  } catch {
    // Not worth a message.
  }
  await check(false);
}

/** Settings → Check for updates now. */
export async function checkForUpdatesNow(): Promise<void> {
  await check(true);
}

async function check(force: boolean): Promise<void> {
  if (busy) return;
  busy = true;
  try {
    kind = await api.installKind();
    const found = await api.checkUpdate(force);
    if (!found?.newer) {
      if (force) toast(`Fosta Studio ${found?.current ?? __APP_VERSION__} is up to date`, "success");
      return;
    }
    info = found;
    if (phase === "none") phase = "available";
    render();
    // Not awaited: a manual check opens the dialog, which follows the download.
    if (kind === "installer" && isTauri() && phase === "available") void download();
    if (force) openUpdateDialog();
  } catch (error) {
    // The automatic check stays quiet (offline, rate limit); a manual one says why.
    if (force) showError(error, "Check for updates");
    else if (errorCode(error) !== "UPDATE_CHECK_FAILED") console.warn("Update check:", error);
  } finally {
    busy = false;
  }
}

/** Installed app: fetches the update in the background; any failure leaves the release link. */
async function download(): Promise<void> {
  phase = "downloading";
  percent = null;
  render();
  try {
    const { check: checkUpdater } = await import("@tauri-apps/plugin-updater");
    const update = await checkUpdater();
    if (!update) throw new Error("No update in latest.json");
    let total: number | null = null;
    let downloaded = 0;
    await update.download((event) => {
      if (event.event === "Started") total = event.data.contentLength ?? null;
      else if (event.event === "Progress") downloaded += event.data.chunkLength;
      const next = downloadPercent(downloaded, total);
      if (next !== percent) {
        percent = next;
        render();
      }
    });
    pending = update;
    phase = "ready";
  } catch (error) {
    console.warn("Update download:", error);
    phase = "available";
  }
  render();
}

/** Runs when the window closes (after the unsaved-work question): installs a downloaded update without relaunching. */
export async function installOnExit(): Promise<void> {
  if (phase !== "ready" || !pending) return;
  try {
    // On Windows this starts the installer and ends the process.
    await pending.install({ restartAfterInstall: false });
  } catch (error) {
    console.warn("Install update on exit:", error);
  }
}

async function restartNow(): Promise<boolean> {
  if (!pending || !host || !(await host.confirmRestart())) return false;
  try {
    await pending.install();
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch (error) {
    phase = "available";
    pending = null;
    render();
    showError(error, "Install the update");
  }
  return false;
}

function openRelease(url: string): void {
  if (isTauri() && capabilities().edition === "desktop") {
    api.openReleasePage(url).catch((error: unknown) => showError(error, "Open the release page"));
  } else {
    window.open(url, "_blank", "noopener");
  }
}

function releaseLink(url: string, text = "Release notes"): HTMLElement {
  return h("button", { class: "link-button", type: "button", onclick: () => openRelease(url) }, text);
}

function commandLine(text: string): HTMLElement {
  const copy = iconButton("copy", "Copy", () => {
    copyText(text).then(
      () => toast("Copied", "success"),
      (error: unknown) => showError(error, "Copy"),
    );
  });
  return h("div", { class: "update-command" }, h("code", { class: "mono" }, text), copy);
}

function dialogBody(update: UpdateInfo): HTMLElement {
  const version = pending?.version ?? update.latest;
  const summary = h("p", {}, `Fosta Studio ${version} is available (you have ${update.current}). `, releaseLink(update.url));
  const body = h("div", { class: "update-dialog" }, summary);
  if (kind === "web") {
    body.append(
      h("p", {}, "Update the container, keeping its data volume:"),
      ...dockerUpdateCommands().map(commandLine),
      h("p", { class: "field-hint" }, "Or let Watchtower do it: see Updating in docs/web-edition.md."),
    );
  } else if (phase === "downloading") {
    body.append(h("progress", { class: "update-progress", max: 100, value: percent ?? undefined }));
  } else if (phase === "ready") {
    body.append(h("p", {}, "Downloaded. It installs when you quit."));
  }
  return body;
}

function openUpdateDialog(): void {
  if (!info) return;
  const update = info;
  const body = h("div", {});
  const shown = phase;
  const draw = () => body.replaceChildren(dialogBody(update));
  draw();
  const actions =
    phase === "ready"
      ? [
          { label: "Later", run: () => undefined },
          { label: "Restart now", primary: true, run: restartNow },
        ]
      : kind === "web" || phase === "downloading"
        ? [{ label: "Close", primary: true, run: () => undefined }]
        : [
            { label: "Later", run: () => undefined },
            { label: "Download", primary: true, run: () => openRelease(update.url) },
          ];
  const close = openModal("Update available", body, actions, {
    width: kind === "web" ? 560 : 420,
    onClose: () => {
      if (refreshDialog === refresh) refreshDialog = null;
    },
  });
  // Progress redraws the body; a new phase (download finished or failed) changes the buttons, so the dialog reopens.
  const refresh = () => {
    if (phase === shown) return draw();
    close();
    openUpdateDialog();
  };
  refreshDialog = refresh;
}

/** Once, on the first start after an update. */
function showWhatsNew(version: string): void {
  const host = document.querySelector(".toasts") ?? document.body.appendChild(h("div", { class: "toasts" }));
  const note = h("div", { class: "toast info whats-new" }, `What's new in ${version}: `, releaseLink(releaseUrl(version)));
  const dismiss = () => note.remove();
  note.addEventListener("click", dismiss);
  host.append(note);
  setTimeout(dismiss, 15000);
}
