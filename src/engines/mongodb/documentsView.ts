/**
 * The Documents section of a collection workspace: the query bar, then one page of the matching documents in the mode
 * picked (List or JSON; Table and the tree come with step M4), paged by `_id` when the sort allows it (`nextAfter`) and
 * by skip otherwise, with the count (estimated from metadata without a filter) and the server time of the page.
 * Every read goes through `mongo.find` / `mongo.count` (`invoke`), so the gateway classifies and may refuse it; its
 * errors are shown in place of the documents. Each run is added to the query history as a shell command.
 */
import { api } from "../../core/api";
import { formatNumber, h, icon, iconButton } from "../../core/dom";
import { renderError } from "../../core/errorPanel";
import { errorText } from "../../core/errors";
import type { HistoryEntry } from "../../core/types";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { SchemaCache } from "../../lang/schemaCache";
import { documentPaths, type Doc } from "./documents";
import { MODES, type DocumentsRenderer } from "./documentModes";
import { MongoApi, type FindResult } from "./mongoApi";
import { QueryBar } from "./queryBar";
import {
  buildQuery,
  commandText,
  EMPTY_QUERY,
  FIELD_LABELS,
  firstCursor,
  nextCursor,
  pageArgs,
  rangeText,
  readSavedState,
  rememberFilter,
  type BuiltQuery,
  type DocumentsMode,
  type DocumentsSavedState,
  type PageCursor,
} from "./queryModel";
import "./mongodb.css";

/** Store keys other sections read (the query of this tab, the field paths seen). */
export const STORE_QUERY = "mongo.query";
export const STORE_FIELD_PATHS = "mongo.fieldPaths";

export function documentsSectionFactory(ctx: SectionContext): SectionContent {
  return new DocumentsSection(ctx);
}

class DocumentsSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly bar: QueryBar;
  private readonly schema = new SchemaCache();
  private readonly database: string;
  private readonly collection: string;
  private readonly modeButtons = new Map<DocumentsMode, HTMLButtonElement>();
  private readonly renderers = new Map<DocumentsMode, DocumentsRenderer>();
  private readonly results = h("div", { class: "mq-results" });
  private readonly message = h("div", { class: "mq-message", hidden: true });
  private readonly range = h("span", { class: "pager-info mq-range" });
  private readonly timing = h("span", { class: "mq-timing" });
  private readonly prev: HTMLButtonElement;
  private readonly next: HTMLButtonElement;
  private mode: DocumentsMode;
  private recent: string[];
  private query: BuiltQuery | null = null;
  /** The cursors of the pages read so far in this run; the last one is shown. */
  private cursors: PageCursor[] = [];
  private nextPage: PageCursor | null = null;
  private page: FindResult | null = null;
  private total: { count: number; estimated: boolean } | null = null;
  private lastError: string | null = null;
  private generation = 0;
  private disposed = false;

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    const saved: DocumentsSavedState = readSavedState(ctx.savedState);
    this.mode = saved.mode;
    this.recent = saved.recent;
    void this.schema.load(() => ctx.view.connection.completionSchema(this.database || null), this.database);

    this.bar = new QueryBar({
      connectionId: ctx.view.connection.connectionId,
      collection: this.collection,
      initial: saved,
      api: this.api,
      schema: this.schema,
      fieldPaths: () => ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? [],
      recent: () => this.recent,
      run: () => void this.run(),
      reset: () => {
        this.bar.setState(EMPTY_QUERY);
        this.ctx.stateChanged();
        void this.run();
      },
      changed: () => this.ctx.stateChanged(),
    });

    const modes = h("div", { class: "segmented mq-modes", role: "group", "aria-label": "Show documents as" });
    for (const spec of MODES) {
      const button = h("button", { class: "segment", type: "button", title: spec.title, "aria-pressed": "false" }, spec.label);
      button.addEventListener("click", () => this.setMode(spec.id));
      this.modeButtons.set(spec.id, button);
      modes.append(button);
    }
    this.prev = iconButton("prev", "Previous page", () => void this.previous());
    this.next = iconButton("chevron", "Next page", () => void this.forward());
    const refresh = iconButton("refresh", "Run again", () => void this.run(false));
    const toolbar = h(
      "div",
      { class: "toolbar mq-results-bar" },
      modes,
      h("div", { class: "toolbar-end" }, this.timing, this.range, this.prev, this.next, refresh),
    );
    this.element = h("div", { class: "view mq-documents" }, this.bar.element, toolbar, h("div", { class: "mq-body" }, this.message, this.results));
    this.setMode(this.mode, false);
    this.updatePager();
    void this.run(false);
  }

  dispose(): void {
    this.disposed = true;
    this.bar.dispose();
    for (const renderer of this.renderers.values()) renderer.dispose?.();
    this.renderers.clear();
  }

  saveState(): DocumentsSavedState {
    return { ...this.bar.state(), mode: this.mode, options: this.bar.optionsOpen(), recent: this.recent };
  }

  assistantContext(): string | null {
    const state = this.bar.state();
    const lines = [`Collection ${this.database}.${this.collection}.`];
    lines.push(`Filter: ${state.filter.trim() || "{} (none)"}`);
    if (state.project.trim()) lines.push(`Project: ${state.project.trim()}`);
    if (state.sort.trim()) lines.push(`Sort: ${state.sort.trim()}`);
    if (state.collation.trim()) lines.push(`Collation: ${state.collation.trim()}`);
    if (state.skip > 0) lines.push(`Skip: ${state.skip}`);
    lines.push(`Limit: ${state.limit}`);
    if (this.page) {
      const total = this.total ? `${this.total.estimated ? "about " : ""}${this.total.count}` : "unknown";
      lines.push(`Shown: ${this.page.documents.length} documents (matching: ${total}).`);
    }
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  /** Runs the bar's query from its first page; `record` adds it to the recent filters and the query history. */
  private async run(record = true): Promise<void> {
    const state = this.bar.state();
    const built = buildQuery(state, { database: this.database, collection: this.collection });
    if (!built.ok) {
      this.bar.showError(built.error);
      this.bar.focusError(built.error);
      this.lastError = `${FIELD_LABELS[built.error.field]}: ${built.error.message}`;
      return;
    }
    this.bar.showError(null);
    this.bar.ran();
    this.query = built.query;
    this.ctx.store.set(STORE_QUERY, state);
    if (record) {
      this.recent = rememberFilter(this.recent, state.filter);
      this.ctx.stateChanged();
    }
    this.total = null;
    this.cursors = [firstCursor(state)];
    const generation = ++this.generation;
    void this.count(built.query, generation);
    const started = Date.now();
    const page = await this.load(this.cursors[0], generation);
    if (record && page !== undefined) this.record(commandText(state, this.collection), started, page);
  }

  private async count(query: BuiltQuery, generation: number): Promise<void> {
    try {
      const result = await this.api.count(query.count);
      if (this.disposed || generation !== this.generation || !result) return;
      this.total = { count: result.count, estimated: result.estimated };
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.total = null;
      this.range.title = `The count failed: ${errorText(error)}`;
    }
    this.updatePager();
  }

  /** Reads the page at `cursor`; resolves the page, null when declined, undefined when stale or failed. */
  private async load(cursor: PageCursor, generation = this.generation): Promise<FindResult | null | undefined | Error> {
    if (!this.query) return undefined;
    this.element.classList.add("busy");
    this.prev.disabled = true;
    this.next.disabled = true;
    try {
      const page = await this.api.find(pageArgs(this.query.find, cursor));
      if (this.disposed || generation !== this.generation) return undefined;
      if (!page) {
        this.showMessage("The query was not run (the confirmation was declined).");
        this.page = null;
        return null;
      }
      this.page = page;
      this.lastError = null;
      this.nextPage = nextCursor(cursor, page);
      const paths = documentPaths(page.documents);
      const known = this.ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? [];
      this.ctx.store.set(STORE_FIELD_PATHS, [...new Set([...known, ...paths])].slice(0, 600));
      this.draw();
      return page;
    } catch (error) {
      if (this.disposed || generation !== this.generation) return undefined;
      this.page = null;
      this.nextPage = null;
      this.lastError = errorText(error);
      this.results.replaceChildren();
      this.message.replaceChildren(renderError(error, { context: `Find in ${this.collection}` }));
      this.message.hidden = false;
      this.timing.textContent = "";
      return error instanceof Error ? error : new Error(errorText(error));
    } finally {
      if (!this.disposed && generation === this.generation) {
        this.element.classList.remove("busy");
        this.updatePager();
      }
    }
  }

  private async forward(): Promise<void> {
    if (!this.nextPage) return;
    const cursor = this.nextPage;
    this.cursors.push(cursor);
    const result = await this.load(cursor);
    if (!result || result instanceof Error) this.cursors.pop();
  }

  private async previous(): Promise<void> {
    if (this.cursors.length < 2) return;
    this.cursors.pop();
    await this.load(this.cursors[this.cursors.length - 1]);
  }

  private draw(): void {
    const page = this.page;
    if (!page) return;
    const documents = page.documents as Doc[];
    this.timing.textContent = `${formatNumber(page.elapsedMs)} ms`;
    this.timing.title = page.statement;
    if (documents.length === 0) {
      const filtered = this.query?.filtered ?? false;
      const first = this.cursors.length <= 1;
      this.results.replaceChildren();
      this.showMessage(
        !first
          ? "No more documents."
          : filtered
            ? "No documents match this filter."
            : this.bar.state().skip > 0
              ? "No documents after the skipped ones."
              : "This collection has no documents.",
        filtered && first,
      );
      return;
    }
    this.message.hidden = true;
    this.message.replaceChildren();
    const renderer = this.renderer(this.mode);
    renderer.show(documents, this.cursors[this.cursors.length - 1]?.offset ?? 0);
    this.results.replaceChildren(renderer.element);
  }

  private showMessage(text: string, offerReset = false): void {
    const reset = offerReset ? h("button", { class: "link-button", type: "button" }, "Clear the filter") : null;
    reset?.addEventListener("click", () => {
      this.bar.setState({ ...this.bar.state(), filter: "" });
      this.ctx.stateChanged();
      void this.run();
    });
    this.message.replaceChildren(h("div", { class: "mq-empty" }, icon("search", "mq-empty-icon"), h("div", {}, text), reset));
    this.message.hidden = false;
  }

  private renderer(mode: DocumentsMode): DocumentsRenderer {
    let renderer = this.renderers.get(mode);
    if (!renderer) {
      const spec = MODES.find((item) => item.id === mode) ?? MODES[0];
      renderer = spec.create();
      this.renderers.set(mode, renderer);
    }
    return renderer;
  }

  private setMode(mode: DocumentsMode, notify = true): void {
    this.mode = mode;
    for (const [id, button] of this.modeButtons) {
      button.classList.toggle("active", id === mode);
      button.setAttribute("aria-pressed", String(id === mode));
    }
    if (this.page && this.page.documents.length > 0) this.draw();
    if (notify) this.ctx.stateChanged();
  }

  private updatePager(): void {
    const shown = this.page?.documents.length ?? 0;
    const offset = this.cursors[this.cursors.length - 1]?.offset ?? 0;
    this.range.textContent = this.page ? rangeText(offset, shown, this.total?.count ?? null, this.total?.estimated ?? false, formatNumber) : "";
    if (this.total?.estimated) this.range.title = "Estimated from the collection's metadata (no filter)";
    else if (this.total) this.range.title = "";
    this.prev.disabled = this.cursors.length < 2;
    this.next.disabled = !this.nextPage;
  }

  /** Adds the run to the query history in the background (turned off: the backend keeps nothing). */
  private record(command: string, started: number, page: FindResult | null | Error): void {
    const view = this.ctx.view;
    const entry: HistoryEntry = {
      id: "",
      sql: command,
      connectionId: view.connection.connectionId,
      connectionName: view.profile.name,
      driver: view.engine.id,
      database: this.database || null,
      startedAt: started,
      durationMs: page && !(page instanceof Error) ? page.elapsedMs : Math.max(0, Date.now() - started),
      results: page && !(page instanceof Error) ? [{ rowSet: true, rows: page.documents.length, affectedRows: 0, truncated: page.hasMore }] : [],
      error: page instanceof Error ? page.message : null,
      cancelled: page === null,
    };
    api.historyAppend(entry).catch(() => {
      // The documents are shown either way; a history that cannot be written is not worth an error per run.
    });
  }
}
