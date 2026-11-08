/**
 * How errors are shown, everywhere in the app:
 *
 * - `renderError(error, options)`: an inline block (code, message, engine, SQLSTATE / vendor code, detail, hint,
 *   line:column that jumps the editor when `onJump` is given, the failing statement, a retryable note and "Copy error").
 *   Views put it where the failure happened (a query's Messages panel, a form).
 * - `showError(error, context)`: the app-level error panel, a dismissible list docked above the status bar, newest
 *   first, with a count badge in the status bar. Use it instead of an error toast; toasts are for success and info.
 */
import { api } from "./api";
import type { AppError, ErrorPosition } from "./contract";
import { toast } from "./dialogs";
import { copyText, h, icon } from "./dom";
import { describeCodes, describePosition, extraFields, formatError, toAppError } from "./errors";

export interface RenderErrorOptions {
  /** Makes line:column a link that moves the editor to the position. */
  onJump?(position: ErrorPosition): void;
  /** One line of message plus the code; details stay behind "Details". */
  compact?: boolean;
  /** What the user was doing; shown and copied. */
  context?: string;
  time?: Date;
}

/** Copies the error with every field and the diagnostics header (diagnostics are skipped when unavailable). */
export async function copyError(error: unknown, context?: string, time?: Date): Promise<void> {
  let diagnostics = null;
  try {
    diagnostics = await api.diagnostics();
  } catch {
    // Copy the error alone.
  }
  try {
    await copyText(formatError(error, { context, diagnostics, time }));
    toast("Error copied", "success");
  } catch {
    toast("Could not write to the clipboard");
  }
}

/** The inline error block. */
export function renderError(error: unknown, options: RenderErrorOptions = {}): HTMLElement {
  const e = toAppError(error);
  const block = h("div", { class: `error-block${options.compact ? " compact" : ""}`, role: "alert" });
  const header = h(
    "div",
    { class: "error-block-header" },
    icon("x", "error-block-icon"),
    h("span", { class: "error-code" }, e.code),
    e.engine ? h("span", { class: "error-engine" }, e.engine) : null,
    options.context ? h("span", { class: "error-context" }, options.context) : null,
    h("span", { class: "spacer" }),
    copyButton(e, options),
  );
  block.append(header, h("div", { class: "error-message" }, e.message));
  const details = errorDetails(e, options);
  if (details.length === 0) return block;
  if (options.compact) {
    const toggle = h("button", { class: "link-button error-more", type: "button" }, "Details");
    const body = h("div", { class: "error-details" }, ...details);
    body.hidden = true;
    toggle.addEventListener("click", () => {
      body.hidden = !body.hidden;
      toggle.textContent = body.hidden ? "Details" : "Hide details";
    });
    block.append(toggle, body);
  } else {
    block.append(h("div", { class: "error-details" }, ...details));
  }
  return block;
}

function copyButton(error: AppError, options: RenderErrorOptions): HTMLButtonElement {
  const button = h(
    "button",
    { class: "btn small error-copy", type: "button", title: "Copy every field of the error and the diagnostics" },
    icon("copy"),
    "Copy error",
  );
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    void copyError(error, options.context, options.time);
  });
  return button;
}

function row(label: string, value: Node | string): HTMLElement {
  return h("div", { class: "error-row" }, h("span", { class: "error-label" }, label), h("span", { class: "error-value" }, value));
}

function errorDetails(e: AppError, options: RenderErrorOptions): HTMLElement[] {
  const rows: HTMLElement[] = [];
  const codes = describeCodes(e);
  if (codes) rows.push(row("Codes", codes));
  if (e.detail) rows.push(row("Detail", e.detail));
  if (e.hint) rows.push(row("Hint", e.hint));
  const position = describePosition(e.position);
  if (position && e.position) {
    const where = e.position;
    const value = options.onJump
      ? h(
          "button",
          { class: "link-button error-jump", type: "button", title: "Show this position in the editor", onclick: () => options.onJump?.(where) },
          position,
        )
      : position;
    rows.push(row("Position", value));
  }
  if (e.statement) rows.push(row(`Statement ${e.statement.index + 1}`, h("code", { class: "error-statement" }, e.statement.text)));
  if (e.cause && e.cause.length > 0) rows.push(row("Cause", e.cause.join(" ← ")));
  if (e.retryable) rows.push(row("Retry", "This error is usually temporary; trying again may work."));
  const extra = extraFields(e);
  if (Object.keys(extra).length > 0) rows.push(row("Other", h("code", { class: "error-extra" }, JSON.stringify(extra))));
  return rows;
}

// --- App-level panel ----------------------------------------------------------------------------------------------------

interface PanelEntry {
  id: number;
  error: AppError;
  context: string | undefined;
  time: Date;
}

const MAX_ENTRIES = 50;

class ErrorPanel {
  readonly element = h("section", { class: "error-panel", "aria-label": "Errors" });
  readonly badge = h("button", { class: "error-badge", type: "button", title: "Show errors" });
  private readonly list = h("div", { class: "error-panel-list" });
  private readonly title = h("span", { class: "error-panel-title" });
  private entries: PanelEntry[] = [];
  private unseen = 0;
  private sequence = 0;

  constructor() {
    this.element.hidden = true;
    this.badge.hidden = true;
    this.badge.addEventListener("click", () => this.toggle());
    const clear = h("button", { class: "btn small", type: "button", title: "Remove every error from the list" }, "Clear all");
    clear.addEventListener("click", () => this.clear());
    const close = h("button", { class: "icon-button", type: "button", title: "Hide the error panel", "aria-label": "Hide the error panel" }, icon("x"));
    close.addEventListener("click", () => this.toggle(false));
    this.element.append(h("div", { class: "error-panel-header" }, icon("logs"), this.title, h("span", { class: "spacer" }), clear, close), this.list);
  }

  add(error: unknown, context: string | undefined, reveal: boolean): void {
    this.sequence += 1;
    this.entries = [{ id: this.sequence, error: toAppError(error), context, time: new Date() }, ...this.entries].slice(0, MAX_ENTRIES);
    if (reveal) this.toggle(true);
    else if (this.element.hidden) this.unseen += 1;
    this.render();
  }

  toggle(open = this.element.hidden): void {
    this.ensureMounted();
    this.element.hidden = !open || this.entries.length === 0;
    if (!this.element.hidden) this.unseen = 0;
    this.render();
  }

  clear(): void {
    this.entries = [];
    this.unseen = 0;
    this.element.hidden = true;
    this.render();
  }

  get count(): number {
    return this.entries.length;
  }

  private remove(id: number): void {
    this.entries = this.entries.filter((entry) => entry.id !== id);
    if (this.entries.length === 0) this.element.hidden = true;
    this.render();
  }

  private render(): void {
    const count = this.entries.length;
    this.title.textContent = count === 1 ? "1 error" : `${count} errors`;
    this.badge.hidden = count === 0;
    this.badge.replaceChildren(icon("x"), h("span", {}, String(count)));
    this.badge.classList.toggle("unseen", this.unseen > 0);
    this.badge.title = this.element.hidden ? `Show errors (${count})` : "Hide errors";
    this.list.replaceChildren(
      ...this.entries.map((entry) => {
        const dismiss = h("button", { class: "icon-button error-dismiss", type: "button", title: "Dismiss", "aria-label": "Dismiss" }, icon("x"));
        dismiss.addEventListener("click", () => this.remove(entry.id));
        return h(
          "div",
          { class: "error-panel-entry" },
          h("span", { class: "error-time", title: entry.time.toLocaleString() }, entry.time.toLocaleTimeString()),
          renderError(entry.error, { compact: true, context: entry.context, time: entry.time }),
          dismiss,
        );
      }),
    );
  }

  /** Docks above the status bar of the main window (or at the end of the page before the layout exists). */
  private ensureMounted(): void {
    if (this.element.isConnected) return;
    const statusBar = document.querySelector(".app-main > .status-bar");
    if (statusBar) statusBar.before(this.element);
    else document.body.append(this.element);
  }
}

let panel: ErrorPanel | null = null;

function errorPanel(): ErrorPanel {
  panel ??= new ErrorPanel();
  return panel;
}

/** The panel element (placed by the shell above the status bar). */
export function errorPanelElement(): HTMLElement {
  return errorPanel().element;
}

/** The status bar badge that shows the error count and toggles the panel. */
export function errorPanelBadge(): HTMLElement {
  return errorPanel().badge;
}

export interface ShowErrorOptions {
  /** False when the view already shows the error inline: the entry is recorded and the badge counts it, the panel stays closed. */
  reveal?: boolean;
}

/** Records an error in the app-level panel and opens it. */
export function showError(error: unknown, context?: string, options: ShowErrorOptions = {}): void {
  console.warn(context ? `${context}:` : "Error:", error);
  errorPanel().add(error, context, options.reveal ?? true);
}
