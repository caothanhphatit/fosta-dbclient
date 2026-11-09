/** Window-level behaviour of the shell: keyboard shortcuts, the context menu, closing and first paint. */
import { getCurrentWindow } from "@tauri-apps/api/window";
import { confirmDialog, isModalOpen } from "../core/dialogs";
import type { TabManager } from "../core/tabs";
import { isTauri } from "../core/transport";
import type { AppConfig } from "../core/types";
import { isQueryView } from "./assistantContext";

/** What the shortcuts trigger. */
export interface ShortcutHost {
  config(): AppConfig | null;
  switchWorkspace(id: string): void;
  newConnection(): void;
  newQuery(): void;
  toggleAssistant(): void;
  toggleSidebar(): void;
}

export function installShortcuts(host: ShortcutHost): void {
  document.addEventListener("keydown", (event) => {
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    // Shortcuts would act behind an open dialog (or stack a second one); keep the browser's own meaning off too.
    if (isModalOpen()) {
      if (/^[1-9ntb]$/.test(key) && !event.shiftKey) event.preventDefault();
      return;
    }
    if (key >= "1" && key <= "9" && !event.shiftKey) {
      const target = host.config()?.workspaces[Number(key) - 1];
      if (target) {
        event.preventDefault();
        host.switchWorkspace(target.id);
      }
    } else if (key === "n" && !event.shiftKey) {
      event.preventDefault();
      host.newConnection();
    } else if (key === "t" && !event.shiftKey) {
      event.preventDefault();
      host.newQuery();
    } else if (key === "b" && !event.shiftKey) {
      event.preventDefault();
      host.toggleSidebar();
    } else if (key === "a" && event.shiftKey) {
      event.preventDefault();
      host.toggleAssistant();
    }
  });
}

export function suppressBrowserContextMenu(): void {
  document.addEventListener("contextmenu", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest("input, textarea, .cm-editor")) return;
    event.preventDefault();
  });
}

export function revealWindowAfterFirstPaint(): void {
  if (isTauri()) requestAnimationFrame(() => requestAnimationFrame(() => void getCurrentWindow().show()));
}

/**
 * Asks before quitting (closing, or restarting to update) when open transactions would be rolled back or unsaved edits
 * lost; true to go ahead. `action` is the button, "Close" or "Restart".
 */
export async function confirmQuit(tabs: TabManager, action: "Close" | "Restart"): Promise<boolean> {
  const contents = tabs.contents();
  const open = contents.filter((content) => isQueryView(content) && content.hasOpenTransaction()).length;
  const edited = contents.filter((content) => content.hasUnsavedChanges?.() === true).length;
  const unrun = unsavedQueryTitles(tabs);
  if (open === 0 && edited === 0 && unrun.length === 0) return true;
  const doing = action === "Close" ? "Closing" : "Restarting";
  const lost = [
    open > 0 ? `${open} query tab(s) have an open transaction. ${doing} Fosta Studio rolls them back, so uncommitted changes are lost.` : "",
    edited > 0 ? `${edited} tab(s) have unsaved edits, which are lost.` : "",
    unrun.length > 0 ? `Query text not run yet: ${unrun.join(", ")}. It is kept and comes back with its tabs the next time Fosta Studio starts.` : "",
  ];
  return confirmDialog(`${lost.filter(Boolean).join("\n")}\n${action} now?`, action);
}

/** Titles of the query tabs (restored ones too) whose text was edited and not run since. */
export function unsavedQueryTitles(tabs: TabManager): string[] {
  return tabs.contents().flatMap((content) => {
    const unsaved = (content as { hasUnsavedText?: () => boolean }).hasUnsavedText;
    if (typeof unsaved !== "function" || !unsaved.call(content)) return [];
    const key = tabs.keyOf(content);
    return key ? [tabs.titleOf(key)] : [];
  });
}

/**
 * The web edition: leaving or reloading the page asks first while a transaction is open, edits are unsaved or query
 * text has not been saved yet (`beforeLeave` saves the open tabs on the way out).
 */
export function guardPageUnload(tabs: TabManager, beforeLeave: () => void): void {
  if (isTauri()) return;
  window.addEventListener("pagehide", beforeLeave);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") beforeLeave();
  });
  window.addEventListener("beforeunload", (event) => {
    beforeLeave();
    const contents = tabs.contents();
    const risky =
      contents.some((content) => (isQueryView(content) && content.hasOpenTransaction()) || content.hasUnsavedChanges?.() === true) ||
      unsavedQueryTitles(tabs).length > 0;
    if (risky) event.preventDefault();
  });
}

/** Closing the window rolls back open transactions and drops unsaved edits; ask first. `beforeExit` runs once agreed. */
export function guardWindowClose(tabs: TabManager, beforeExit: () => Promise<void> = async () => {}): void {
  // The browser preview (dev mock) has no window to guard.
  if (!("__TAURI_INTERNALS__" in window)) return;
  void getCurrentWindow().onCloseRequested(async (event) => {
    if (!(await confirmQuit(tabs, "Close"))) {
      event.preventDefault();
      return;
    }
    await beforeExit();
  });
}
