/**
 * The schema compare tab: reads both sides (`schema_snapshot`, in parallel, cancellable), diffs them (`schema_diff`, pure:
 * options and Swap re-diff without reading again) and shows the grouped list, the details of one item and, after
 * Generate script, the sync script with its warnings. Nothing runs here: "Open in query tab" puts the script in a query
 * tab on the target connection, where the workspace's rules apply.
 */
import "./compare.css";
import { api } from "../../core/api";
import type { CompareOptions, DiffItem, SchemaDiff, SchemaSnapshot, SyncScript } from "../../core/contract";
import { checkbox, textInput, toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton, separator, spacer, toolButton } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import { makeSplitter } from "../../core/splitter";
import type { TabContent } from "../../core/tabs";
import type { SafetySettings } from "../../core/types";
import { savedDialog } from "../importExport/importExport";
import { safetyLabel } from "../safety/safety";
import {
  carrySelection,
  defaultSelection,
  includedIds,
  OPTION_TOGGLES,
  safetyNotice,
  scriptFileName,
  sideName,
  summary,
  swapped,
  warningsOf,
  type CompareRequest,
  type CompareSide,
} from "./compareModel";
import { CompareDetails } from "./compareDetails";
import { CompareList } from "./compareList";
import { highlightSql } from "./ddlDiff";

export interface CompareHost {
  connectionName(connectionId: string): string;
  /** Connects the connection (of any workspace) unless it is connected; true once it is. */
  connect(connectionId: string): Promise<boolean>;
  /** The rules a connection runs under, and whether its workspace is marked Production. */
  safety(connectionId: string): { settings: SafetySettings | null; production: boolean };
  openQuery(connectionId: string, database: string | null, sql: string): void;
  /** "Change…": the picker with this compare's sides. */
  pick(request: CompareRequest): void;
  /** The request changed (Swap, options): saved with the open tabs, and the tab title follows. */
  changed(request: CompareRequest): void;
}

const SCRIPT_MIN = 120;

export class CompareView implements TabContent {
  readonly element: HTMLElement;
  private request: CompareRequest;
  private snapshots: { source: SchemaSnapshot; target: SchemaSnapshot } | null = null;
  private diff: SchemaDiff | null = null;
  private script: SyncScript | null = null;
  private readonly list: CompareList;
  private readonly details = new CompareDetails();
  private readonly sides = h("div", { class: "compare-sides" });
  private readonly summaryText = h("span", { class: "compare-summary" });
  private readonly notices = h("div", { class: "compare-notices" });
  private readonly optionsPanel = h("div", { class: "compare-options" });
  private readonly filter = textInput("", { class: "input compare-filter", placeholder: "Filter objects", "aria-label": "Filter objects" });
  private readonly identical = checkbox("Show identical", false);
  private readonly generate = h("button", { class: "btn primary compare-generate", type: "button" }, icon("play"), h("span", {}, "Generate script"));
  private readonly progress = h("div", { class: "compare-progress" });
  private readonly scriptPanel = h("div", { class: "compare-script" });
  private readonly scriptSplitter = h("div", { class: "splitter-y compare-script-splitter" });
  private readonly scriptMeta = h("span", { class: "compare-script-meta" });
  private readonly scriptText = h("pre", { class: "ddl compare-script-text" });
  private readonly scriptWarnings = h("ul", { class: "compare-script-warnings" });
  private readonly transactional = checkbox("Wrap in a transaction", true);
  /** Read requests of the compare in progress (`cancelRequest`); null when none runs. */
  private reading: string[] | null = null;
  /** Bumped by every compare, re-diff and script; a result of an older one is dropped. */
  private generation = 0;
  private scriptTimer: ReturnType<typeof setTimeout> | null = null;
  private disposed = false;

  constructor(
    request: CompareRequest,
    private readonly host: CompareHost,
    autoRun: boolean,
  ) {
    this.request = request;
    this.list = new CompareList({
      selectionChanged: () => this.selectionChanged(),
      activated: (item) => this.showDetails(item),
    });
    this.filter.addEventListener("input", () => this.list.setFilter(this.filter.value));
    this.identical.input.addEventListener("change", () => this.list.setShowIdentical(this.identical.input.checked));
    this.generate.addEventListener("click", () => void this.generateScript(true));
    this.transactional.input.addEventListener("change", () => {
      this.request = { ...this.request, options: { ...this.request.options, transactional: this.transactional.input.checked } };
      this.host.changed(this.request);
      void this.generateScript(false);
    });
    const optionsButton = toolButton("filter", "Options", () => this.toggleOptions(), "What the compare ignores");
    const toolbar = h(
      "div",
      { class: "toolbar compare-toolbar" },
      this.sides,
      separator(),
      toolButton("compare", "Swap", () => void this.swap(), "Swap source and target (re-diffs without reading again)"),
      toolButton("refresh", "Compare again", () => void this.compare(), "Read both schemas again and compare"),
      toolButton("edit", "Change…", () => this.host.pick(this.request), "Pick other connections, databases or schemas"),
      optionsButton,
      separator(),
      this.filter,
      this.identical.element,
      this.summaryText,
      spacer(),
      this.generate,
    );
    const listSplitter = h("div", { class: "splitter-x compare-list-splitter" });
    const main = h("div", { class: "compare-main" }, this.list.element, listSplitter, this.details.element);
    this.buildScriptPanel();
    this.element = h(
      "div",
      { class: "view compare-view" },
      toolbar,
      this.notices,
      this.optionsPanel,
      main,
      this.scriptSplitter,
      this.scriptPanel,
      this.progress,
    );
    let listWidth = 340;
    makeSplitter(listSplitter, "x", (delta) => {
      listWidth = Math.min(Math.max(listWidth + delta, 220), 720);
      main.style.setProperty("--compare-list-width", `${listWidth}px`);
    });
    let scriptHeight = 260;
    makeSplitter(this.scriptSplitter, "y", (delta) => {
      scriptHeight = Math.max(scriptHeight - delta, SCRIPT_MIN);
      this.element.style.setProperty("--compare-script-height", `${scriptHeight}px`);
    });
    this.optionsPanel.hidden = true;
    this.showScript(false);
    this.progress.hidden = true;
    this.renderSides();
    this.renderOptions();
    this.updateSummary();
    if (autoRun) void this.compare();
    else this.idle();
  }

  /** What this tab compares now (saved with the open tabs). */
  compareRequest(): CompareRequest {
    return this.request;
  }

  dispose(): void {
    this.disposed = true;
    this.cancelReads();
    if (this.scriptTimer) clearTimeout(this.scriptTimer);
    this.details.dispose();
  }

  // --- Reading and diffing ---------------------------------------------------------------------------------------------

  /** Reads both schemas (in parallel) and diffs them. */
  async compare(): Promise<void> {
    this.cancelReads();
    const generation = ++this.generation;
    const { source, target } = this.request;
    const ids = [crypto.randomUUID(), crypto.randomUUID()];
    this.reading = ids;
    this.busy(`Reading ${this.describe(source)} and ${this.describe(target)}…`);
    try {
      const connected = await Promise.all([this.host.connect(source.connectionId), this.host.connect(target.connectionId)]);
      if (generation !== this.generation) return;
      if (!connected.every(Boolean)) {
        this.idle("Connect both connections, then compare again.");
        return;
      }
      const [sourceSnapshot, targetSnapshot] = await Promise.all([
        api.schemaSnapshot(source.connectionId, { database: source.database, schema: source.schema }, ids[0]),
        api.schemaSnapshot(target.connectionId, { database: target.database, schema: target.schema }, ids[1]),
      ]);
      if (generation !== this.generation) return;
      this.snapshots = { source: sourceSnapshot, target: targetSnapshot };
      this.reading = null;
      this.done();
      await this.rediff(generation);
    } catch (error) {
      if (generation !== this.generation) return;
      this.reading = null;
      this.idle("The compare did not finish. Fix the problem, then compare again.");
      showError(error, "Compare schemas");
    }
  }

  /** Diffs the snapshots already read with the current options. */
  private async rediff(generation = ++this.generation): Promise<void> {
    if (!this.snapshots) return;
    const previous = this.diff;
    const previousSelection = this.list.selection;
    try {
      const diff = await api.schemaDiff(this.snapshots.source, this.snapshots.target, this.request.options);
      if (generation !== this.generation || this.disposed) return;
      this.diff = diff;
      const selection = previous ? carrySelection(previous.items, previousSelection, diff.items) : defaultSelection(diff.items);
      const active = this.list.activeId();
      this.list.setItems(diff.items, selection, diff.skipped);
      const next = (active && diff.items.find((item) => item.id === active)) || this.list.firstShown();
      if (next) this.list.activate(next.id);
      else this.details.clear(diff.items.length === 0 ? "Neither side has tables or views to compare" : "Both sides are the same");
      this.renderNotices();
      this.updateSummary();
      if (this.scriptVisible()) await this.generateScript(false);
    } catch (error) {
      if (generation !== this.generation) return;
      showError(error, "Compare schemas");
    }
  }

  private async swap(): Promise<void> {
    this.request = swapped(this.request);
    if (this.snapshots) this.snapshots = { source: this.snapshots.target, target: this.snapshots.source };
    // The statuses flip (created ↔ dropped): every item takes its default selection again.
    this.diff = null;
    this.renderSides();
    this.host.changed(this.request);
    if (this.snapshots) await this.rediff();
    else this.renderNotices();
  }

  private cancelReads(): void {
    for (const id of this.reading ?? []) void api.cancelRequest(id).catch(() => undefined);
    this.reading = null;
  }

  // --- Script ----------------------------------------------------------------------------------------------------------

  private selectionChanged(): void {
    this.updateSummary();
    if (!this.scriptVisible()) return;
    if (this.scriptTimer) clearTimeout(this.scriptTimer);
    this.scriptTimer = setTimeout(() => void this.generateScript(false), 250);
  }

  /** `reveal`: the Generate script button (opens the panel); otherwise the shown script follows the selection. */
  private async generateScript(reveal: boolean): Promise<void> {
    if (!this.snapshots || !this.diff) {
      toast("Compare first, then generate the script");
      return;
    }
    const include = includedIds(this.diff.items, this.list.selection);
    const generation = this.generation;
    try {
      const script = await api.schemaSyncScript(this.snapshots.source, this.snapshots.target, this.request.options, include);
      if (generation !== this.generation || this.disposed) return;
      this.script = script;
      this.renderScript(include.length);
      if (reveal) this.showScript(true);
    } catch (error) {
      showError(error, "Generate the sync script");
    }
  }

  private buildScriptPanel(): void {
    const target = () => this.request.target;
    const open = h(
      "button",
      { class: "btn primary small", type: "button", title: "Open the script in a query tab on the target; nothing runs until you run it" },
      icon("query"),
      h("span", {}, "Open in query tab"),
    );
    open.addEventListener("click", () => {
      if (!this.script) return;
      this.host.openQuery(target().connectionId, target().database, this.script.sql);
    });
    const save = toolButton("download", "Save .sql", () => void this.saveScript());
    const copy = toolButton("copy", "Copy", () => {
      if (this.script) void copyText(this.script.sql).then(() => toast("Script copied", "success"));
    });
    this.scriptPanel.append(
      h(
        "div",
        { class: "compare-script-bar" },
        h("span", { class: "compare-script-title" }, "Sync script"),
        this.scriptMeta,
        spacer(),
        this.transactional.element,
        open,
        copy,
        save,
        iconButton("x", "Hide the script", () => this.showScript(false)),
      ),
      h("div", { class: "compare-script-body" }, this.scriptWarnings, this.scriptText),
    );
  }

  private renderScript(selected: number): void {
    const script = this.script;
    if (!script) return;
    const warnings = script.warnings.length;
    this.scriptMeta.textContent = [
      `${selected} ${selected === 1 ? "object" : "objects"}`,
      `${script.statements} ${script.statements === 1 ? "statement" : "statements"}`,
      warnings ? `${warnings} ${warnings === 1 ? "warning" : "warnings"}` : "no warnings",
      script.transactional ? "in a transaction" : "not in a transaction",
    ].join(" · ");
    this.transactional.input.checked = script.transactional;
    this.scriptText.replaceChildren(highlightSql(script.sql));
    const names = new Map((this.diff?.items ?? []).map((item) => [item.id, item.name]));
    this.scriptWarnings.hidden = warnings === 0;
    this.scriptWarnings.replaceChildren(
      ...script.warnings.map((warning) => {
        const row = h(
          "li",
          { class: `risk-${warning.risk}`, title: "Show this object" },
          h("span", { class: "compare-warning-item" }, names.get(warning.itemId) ?? warning.itemId),
          h("span", {}, warning.message),
        );
        row.addEventListener("click", () => this.list.activate(warning.itemId));
        return row;
      }),
    );
    const active = this.list.activeId();
    const item = active ? this.diff?.items.find((candidate) => candidate.id === active) : undefined;
    if (item) this.showDetails(item);
  }

  private async saveScript(): Promise<void> {
    if (!this.script) return;
    try {
      savedDialog("Script saved", await api.saveDownload(scriptFileName(new Date(), this.request.target.database), this.script.sql, null));
    } catch (error) {
      showError(error, "Save the script");
    }
  }

  private scriptVisible(): boolean {
    return !this.scriptPanel.hidden;
  }

  private showScript(shown: boolean): void {
    this.scriptPanel.hidden = !shown;
    this.scriptSplitter.hidden = !shown;
  }

  // --- Rendering -------------------------------------------------------------------------------------------------------

  private showDetails(item: DiffItem): void {
    const warnings = this.script ? warningsOf(this.script.warnings, item.id) : [];
    this.details.show(item, warnings, { target: this.describe(this.request.target), source: this.describe(this.request.source) });
  }

  /** "Staging PostgreSQL · shop.public" */
  private describe(side: CompareSide): string {
    return `${this.host.connectionName(side.connectionId)} · ${sideName(side)}`;
  }

  private renderSides(): void {
    const { source, target } = this.request;
    const safety = this.host.safety(target.connectionId);
    const chip = (role: string, side: CompareSide, badge: HTMLElement | null) =>
      h(
        "span",
        { class: "compare-side", title: `${role}: ${this.describe(side)}` },
        h("span", { class: "compare-role" }, role),
        h("span", { class: "compare-side-connection" }, this.host.connectionName(side.connectionId)),
        h("span", { class: "compare-side-scope" }, sideName(side)),
        badge,
      );
    const badge = safety.settings
      ? h(
          "span",
          { class: `compare-safety preset-${safety.settings.preset}${safety.production ? " production" : ""}` },
          safety.production ? "Production" : safetyLabel(safety.settings),
        )
      : null;
    this.sides.replaceChildren(chip("Source", source, null), h("span", { class: "compare-arrow" }, "→"), chip("Target", target, badge));
    this.renderNotices();
  }

  private renderNotices(): void {
    const safety = this.host.safety(this.request.target.connectionId);
    const notices = safetyNotice(safety.settings, safety.production);
    const diff = this.diff;
    if (diff) {
      const major = (version: string) => version.split(/[.\s]/)[0];
      if (diff.source.serverVersion && diff.target.serverVersion && major(diff.source.serverVersion) !== major(diff.target.serverVersion)) {
        notices.push({
          tone: "info",
          text: `The source runs ${diff.source.serverVersion} and the target ${diff.target.serverVersion}; the script uses the target's dialect.`,
        });
      }
    }
    this.notices.hidden = notices.length === 0;
    this.notices.replaceChildren(
      ...notices.map((notice) =>
        h("div", { class: `compare-notice ${notice.tone}` }, icon(notice.tone === "danger" ? "shield" : "bolt"), h("span", {}, notice.text)),
      ),
    );
  }

  private toggleOptions(): void {
    this.optionsPanel.hidden = !this.optionsPanel.hidden;
  }

  private renderOptions(): void {
    const options = this.request.options;
    const toggles = OPTION_TOGGLES.map((toggle) => {
      const box = checkbox(toggle.label, Boolean(options[toggle.key]));
      box.element.title = toggle.hint;
      box.input.addEventListener("change", () => this.setOption({ [toggle.key]: box.input.checked }));
      return box.element;
    });
    const exclude = textInput(options.exclude.join(", "), { class: "input mono compare-exclude", "aria-label": "Excluded tables" });
    exclude.addEventListener("change", () =>
      this.setOption({
        exclude: exclude.value
          .split(",")
          .map((pattern) => pattern.trim())
          .filter(Boolean),
      }),
    );
    this.optionsPanel.replaceChildren(
      h("div", { class: "compare-option-toggles" }, ...toggles),
      h(
        "label",
        { class: "compare-exclude-field" },
        h("span", {}, "Exclude"),
        exclude,
        h("span", { class: "field-hint" }, "Comma-separated name patterns (* and ?); migration-tool tables are excluded by default"),
      ),
    );
  }

  private setOption(change: Partial<CompareOptions>): void {
    this.request = { ...this.request, options: { ...this.request.options, ...change } };
    this.host.changed(this.request);
    void this.rediff();
  }

  private updateSummary(): void {
    this.summaryText.textContent = this.diff ? summary(this.diff.items, this.list.selection) : "";
    const selected = this.diff ? includedIds(this.diff.items, this.list.selection).length : 0;
    this.generate.disabled = !this.diff || (selected === 0 && !this.scriptVisible());
  }

  private busy(message: string): void {
    const cancel = h("button", { class: "btn", type: "button" }, "Cancel");
    cancel.addEventListener("click", () => {
      this.generation += 1;
      this.cancelReads();
      this.idle("Cancelled. Compare again to read both schemas.");
    });
    this.progress.replaceChildren(h("div", { class: "compare-progress-box" }, h("span", { class: "compare-spinner" }), h("span", {}, message), cancel));
    this.progress.hidden = false;
    this.element.classList.add("busy");
  }

  private done(): void {
    this.progress.hidden = true;
    this.element.classList.remove("busy");
  }

  /** Nothing read (a restored tab, a cancelled or failed compare): one button to compare. */
  private idle(message = "This compare was restored from the last session. Compare again to read both schemas."): void {
    this.done();
    if (this.diff) return;
    const again = h("button", { class: "btn primary", type: "button" }, icon("refresh"), h("span", {}, "Compare again"));
    again.addEventListener("click", () => void this.compare());
    this.details.clear("");
    this.list.setItems([], new Set(), []);
    this.progress.replaceChildren(h("div", { class: "compare-progress-box idle" }, h("span", {}, message), again));
    this.progress.hidden = false;
  }
}
