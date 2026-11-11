/**
 * The Documents section of a collection workspace: the query bar, then one page of the matching documents in the mode
 * picked (List, JSON or Table, `documentModes.ts`), paged by `_id` when the sort allows it (`nextAfter`) and by skip
 * otherwise, with the count (estimated from metadata without a filter, exact on a view) and the server time of the
 * page. Switching modes keeps the document at the top (`anchor.ts`). Inferred references (`refs.ts`) are probed in
 * the background; opening one shows the referenced document in the side panel, with a link to its collection.
 * Every read goes through `mongo.find` / `mongo.count` / `mongo.resolveRef` (`invoke`), so the gateway classifies and
 * may refuse it; its errors are shown in place of the documents. Each run is added to the query history as a shell
 * command.
 *
 * On a collection where the rules allow writes, the documents can be edited (`documentEditing.ts`): edits stay pending,
 * shown in the views and the pending bar, until Save sends them through `apply_edits`. The query bar's More menu updates
 * or deletes every document the filter matches (`bulkDialogs.ts`), after a preview.
 *
 * The assistant's suggestions (a `mongo-filter` block applied, `open_collection_workspace`) fill the bar without running
 * it, after a diff when they replace the user's own text; the bar's "Ask" sends a sentence to the assistant.
 */
import { api } from "../../core/api";
import { suggestionOf, type Suggestion } from "../../core/assistantFences";
import type { ObjectRef } from "../../core/contract";
import type { MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { formatNumber, h, icon, iconButton } from "../../core/dom";
import { renderError } from "../../core/errorPanel";
import { errorText } from "../../core/errors";
import type { HistoryEntry, SavedQuery } from "../../core/types";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { SchemaCache } from "../../lang/schemaCache";
import { anchorAt, indexOfAnchor, type Anchor } from "./anchor";
import { readable, shellText } from "./bson";
import { openDeleteMatching, openUpdateMatching, type BulkContext } from "./bulkDialogs";
import { DocTree } from "./docTree";
import { DocumentEditing } from "./documentEditing";
import { documentPaths, type Doc, type Json } from "./documents";
import { MODES, type DocumentsRenderer, type RendererContext } from "./documentModes";
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
  type QueryState,
} from "./queryModel";
import { filterQuestion, needsReview, sampledDocumentsText } from "./assistantModel";
import { refFields, RefResolver } from "./refs";
import { reviewChange } from "./reviewDialog";
import { stagesFromQuery } from "./pipelineModel";
import { pipelineOfSaved, readQueryRequest, stateOfSaved, type AggregationsRequest, type DocumentsRequest } from "./savedQueries";
import { SavedQueries } from "./savedQueriesMenu";
import "./mongodb.css";

/** Store keys other sections read (the query of this tab, the field paths seen). */
export const STORE_QUERY = "mongo.query";
export const STORE_FIELD_PATHS = "mongo.fieldPaths";

const READ_ONLY_NOTE = "Read-only workspace: documents cannot be edited, added or deleted here. Change the workspace's safety rules to edit.";

export function documentsSectionFactory(ctx: SectionContext): SectionContent {
  return new DocumentsSection(ctx);
}

class DocumentsSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly bar: QueryBar;
  private readonly schema = new SchemaCache();
  private readonly schemaReady: Promise<unknown>;
  private readonly database: string;
  private readonly collection: string;
  /** A view has no metadata count: its total is counted. */
  private readonly isView: boolean;
  private readonly modeButtons = new Map<DocumentsMode, HTMLButtonElement>();
  private readonly renderers = new Map<DocumentsMode, DocumentsRenderer>();
  /** The page each renderer shows now (switching back to a mode does not draw it again). */
  private readonly shown = new Map<DocumentsMode, FindResult>();
  private readonly results = h("div", { class: "mq-results" });
  private readonly message = h("div", { class: "mq-message", hidden: true });
  private readonly staleNote = h("div", { class: "mq-stale-note", role: "status", hidden: true });
  private readonly side: SidePanel;
  private readonly range = h("span", { class: "pager-info mq-range" });
  private readonly timing = h("span", { class: "mq-timing" });
  private readonly prev: HTMLButtonElement;
  private readonly next: HTMLButtonElement;
  private readonly refs: RefResolver;
  private readonly rendererContext: RendererContext;
  /** Editing (List and Table modes); null on a view and where the rules block writes. */
  private readonly editing: DocumentEditing | null;
  private readonly writesBlocked: boolean;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
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
  /** The document the current mode was brought to when it was shown (`setMode`), and whether the user moved since. */
  private anchor: Anchor | null = null;
  private userMoved = false;
  private disposed = false;

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.isView = object?.kind === "view";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    // Edits are offered on collections where the rules allow writes; the gateway checks every save anyway.
    this.writesBlocked = ctx.view.navigator.writesBlocked(ctx.view.connection.connectionId);
    this.editing =
      object && !this.isView && !this.writesBlocked
        ? new DocumentEditing({
            api: this.api,
            object,
            documents: () => (this.page?.documents ?? []) as Doc[],
            changed: () => this.editsChanged(),
            reload: () => this.reload(),
          })
        : null;
    const saved: DocumentsSavedState = readSavedState(ctx.savedState);
    this.mode = saved.mode;
    this.recent = saved.recent;
    this.schemaReady = this.schema.load(() => ctx.view.connection.completionSchema(this.database || null), this.database);
    this.refs = new RefResolver(
      (collection, id) => this.api.resolveRef(collection, id, false),
      async () => {
        await this.schemaReady.catch(() => undefined);
        return this.collectionsOfDatabase().map((table) => table.name);
      },
      this.collection,
    );
    this.rendererContext = {
      refs: {
        targetOf: (path) => this.refs.known(path) ?? null,
        open: (collection, id, path) => void this.side.openRef(collection, id, path),
      },
      inspect: (title, value) => this.side.inspect(title, value),
      edit: this.editing?.tree ?? null,
    };
    this.side = new SidePanel({
      resolve: async (collection, id) => {
        try {
          const cached = await this.refs.document(collection, id);
          if (cached) return cached;
        } catch {
          // Probing does not ask; opening may (a workspace that asks before reads).
        }
        return this.api.resolveRef(collection, id, true);
      },
      openCollection: (collection, id) => this.openCollection(collection, id),
      refs: this.rendererContext.refs,
    });

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
      actions: () => this.moreActions(),
      ask: ctx.view.navigator.askAssistant
        ? (request) =>
            ctx.view.navigator.askAssistant?.(
              filterQuestion(this.collection, request),
              this.assistantContext() ?? undefined,
              this.assistantSamples() ?? undefined,
            )
        : undefined,
      saved: new SavedQueries({
        connectionId: ctx.view.connection.connectionId,
        database: this.database,
        collection: this.collection,
        state: () => this.bar.state(),
        apply: (state) => this.applyQuery(state),
        applyPipeline: (pipeline) => void ctx.deliver("aggregations", { pipeline } satisfies AggregationsRequest),
        openElsewhere: (saved) => this.openSaved(saved),
      }),
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
    const insert = this.editing ? iconButton("plus", "Insert a document…", () => void this.editing?.insert()) : null;
    const toolbar = h(
      "div",
      { class: "toolbar mq-results-bar" },
      modes,
      h("div", { class: "toolbar-end" }, insert, this.timing, this.range, this.prev, this.next, refresh),
    );
    const main = h("div", { class: "mq-main" }, this.staleNote, this.message, this.results);
    // What the user does in the documents (not the scrolling a mode switch does) moves the anchor.
    for (const type of ["wheel", "keydown", "pointerdown"]) this.results.addEventListener(type, () => (this.userMoved = true), { passive: true });
    const readOnlyBar =
      !this.isView && this.writesBlocked ? h("div", { class: "readonly-bar", role: "status" }, icon("view"), h("span", {}, READ_ONLY_NOTE)) : null;
    this.element = h(
      "div",
      { class: "view mq-documents" },
      this.bar.element,
      readOnlyBar,
      toolbar,
      h("div", { class: "mq-body" }, main, this.side.element),
      this.editing?.bar ?? null,
    );
    this.setMode(this.mode, false);
    this.updatePager();
    void this.run(false);
  }

  dispose(): void {
    this.disposed = true;
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.bar.dispose();
    this.side.dispose();
    for (const renderer of this.renderers.values()) renderer.dispose?.();
    this.renderers.clear();
  }

  saveState(): DocumentsSavedState {
    return { ...this.bar.state(), mode: this.mode, options: this.bar.optionsOpen(), recent: this.recent };
  }

  hasUnsavedChanges(): boolean {
    return this.editing !== null && !this.editing.edits.isEmpty();
  }

  closeWarning(): string | null {
    return this.editing?.warning() ?? null;
  }

  /** The page's documents as the views show them: with the pending edits. */
  private shownDocuments(): Doc[] {
    const documents = (this.page?.documents ?? []) as Doc[];
    return this.editing ? this.editing.shown(documents) : documents;
  }

  /** The pending edits changed: the current mode shows the documents again in place; the others draw on their next show. */
  private editsChanged(): void {
    if (!this.page || this.page.documents.length === 0) return;
    for (const mode of [...this.shown.keys()]) if (mode !== this.mode) this.shown.delete(mode);
    const renderer = this.renderers.get(this.mode);
    if (renderer && this.shown.get(this.mode) === this.page) {
      if (renderer.replace) renderer.replace(this.shownDocuments());
      else this.shown.delete(this.mode);
    }
    if (!this.shown.has(this.mode)) this.draw(null);
  }

  /** Reads the current page again (after a save) and counts again. */
  private async reload(): Promise<void> {
    const cursor = this.cursors[this.cursors.length - 1];
    if (!this.query || !cursor) return;
    void this.count(this.query, this.generation);
    await this.load(cursor);
  }

  /** The query bar's More menu: the query as a pipeline, then (on a collection) the bulk actions on the matching documents. */
  private moreActions(): MenuItem[] {
    const open: MenuItem = {
      label: "Open as aggregation",
      icon: "layers",
      detail: "Filter, sort, skip, limit and project as stages",
      run: () => this.openAsAggregation(),
    };
    return this.isView ? [open] : [open, "separator", ...this.bulkActions()];
  }

  /** Turns the bar's query (as typed now) into stages and opens them in the Aggregations section. */
  private openAsAggregation(): void {
    const state = this.bar.state();
    const built = buildQuery(state, { database: this.database, collection: this.collection });
    if (!built.ok) {
      this.bar.showError(built.error);
      this.bar.focusError(built.error);
      return;
    }
    if (state.collation.trim()) toast("The collation is not a stage: the pipeline runs without it.");
    const pipeline = stagesFromQuery(state);
    void this.ctx.deliver("aggregations", { pipeline: pipeline.length > 0 ? pipeline : ["{ $match: {} }"] } satisfies AggregationsRequest);
  }

  private bulkActions(): MenuItem[] {
    const blocked = this.writesBlocked ? "The workspace is Read-only" : undefined;
    return [
      { label: "Update matching documents…", icon: "edit", detail: blocked ?? "Preview first, then update", run: () => this.bulk("update") },
      { label: "Delete matching documents…", icon: "trash", danger: true, disabled: this.writesBlocked, detail: blocked, run: () => this.bulk("delete") },
    ];
  }

  /** Opens the bulk update or delete for the bar's current filter (as typed now, parsed like a run). */
  private bulk(kind: "update" | "delete"): void {
    const state = this.bar.state();
    const built = buildQuery(state, { database: this.database, collection: this.collection });
    if (!built.ok) {
      this.bar.showError(built.error);
      this.bar.focusError(built.error);
      return;
    }
    if (built.query.find.collation) {
      toast("Clear the collation first: bulk updates and deletes run without one, so they could match other documents.", "error");
      return;
    }
    const context: BulkContext = {
      api: this.api,
      collection: this.collection,
      filter: built.query.find.filter ?? null,
      filterText: state.filter,
      done: () => void this.run(false),
    };
    if (kind === "update") openUpdateMatching(context);
    else openDeleteMatching(context);
  }

  /**
   * Runs what another tab hands over: a filter (a reference opened in its collection) or a whole bar state (a saved
   * query of this collection, a query-history entry reopened here).
   */
  receive(request: unknown): void {
    const suggestion = suggestionOf(request);
    if (suggestion) {
      void this.applySuggestion(suggestion);
      return;
    }
    const asked = typeof request === "object" && request !== null ? (request as DocumentsRequest) : {};
    const query = readQueryRequest(asked.query);
    if (query) this.applyQuery(query);
    else if (typeof asked.filter === "string") this.applyQuery({ ...EMPTY_QUERY, limit: this.bar.state().limit, filter: asked.filter });
  }

  /**
   * The assistant's filter, sort or projection: put into the bar and marked, never run. Replacing text of the user's own
   * shows the change first.
   */
  private async applySuggestion(suggestion: Suggestion): Promise<void> {
    const current = this.bar.state();
    const fields = (["filter", "sort", "project"] as const).filter((field) => typeof suggestion[field] === "string" && suggestion[field] !== current[field]);
    if (fields.length === 0) {
      this.bar.focus();
      return;
    }
    const next: QueryState = { ...current };
    for (const field of fields) next[field] = (suggestion[field] ?? "").trim();
    const text = (state: QueryState) => fields.map((field) => `// ${FIELD_LABELS[field]}\n${state[field].trim() || "(empty)"}`).join("\n\n");
    if (fields.some((field) => needsReview(current[field], next[field]))) {
      const accepted = await reviewChange(`Replace the ${fields.map((field) => FIELD_LABELS[field].toLowerCase()).join(" and ")}?`, text(current), text(next));
      if (!accepted || this.disposed) return;
    }
    this.bar.setState(next);
    this.bar.markSuggested(fields);
    this.ctx.stateChanged();
    this.bar.focus();
  }

  /** Puts `state` into the bar (the options row opens when it has options) and runs it. */
  private applyQuery(state: QueryState): void {
    this.bar.setState(state);
    this.ctx.stateChanged();
    void this.run();
  }

  /** A saved query of another collection of the database: opened in that collection's workspace. */
  private openSaved(saved: SavedQuery): void {
    const kind = this.collectionsOfDatabase().find((table) => table.name === saved.collection)?.kind === "view" ? "view" : "collection";
    const object: ObjectRef = { database: this.database, schema: null, name: saved.collection, kind };
    const pipeline = pipelineOfSaved(saved);
    if (pipeline) {
      const request: AggregationsRequest = { pipeline };
      this.ctx.view.navigator.openWorkspace(this.ctx.view.connection.connectionId, object, "aggregations", request);
      return;
    }
    const request: DocumentsRequest = { query: stateOfSaved(saved) };
    this.ctx.view.navigator.openWorkspace(this.ctx.view.connection.connectionId, object, "documents", request);
  }

  /** The first documents shown, cut per field (they reach the model only where the assistant may read data). */
  assistantSamples(): string | null {
    return this.page ? sampledDocumentsText(this.page.documents, `Documents shown in ${this.collection} (first of the page, values cut)`) : null;
  }

  assistantContext(): string | null {
    const state = this.bar.state();
    const lines = [`${this.isView ? "View" : "Collection"} ${this.database}.${this.collection}.`];
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

  private collectionsOfDatabase(): { name: string; kind: string }[] {
    return this.schema.get().tables.filter((table) => table.kind === "collection" || table.kind === "view" || table.kind === "table");
  }

  /** Runs the bar's query from its first page; `record` adds it to the recent filters and the query history. */
  private async run(record = true): Promise<void> {
    const state = this.bar.state();
    const built = buildQuery(state, { database: this.database, collection: this.collection }, { exactCount: this.isView });
    if (!built.ok) {
      this.bar.showError(built.error);
      this.bar.focusError(built.error);
      this.lastError = `${FIELD_LABELS[built.error.field]}: ${built.error.message}`;
      this.markStale(FIELD_LABELS[built.error.field]);
      return;
    }
    this.markStale(null);
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

  /** Dims the documents shown and says they answer the previous query (a field does not parse); null clears it. */
  private markStale(field: string | null): void {
    const stale = field !== null && this.page !== null;
    this.results.classList.toggle("mq-stale", stale);
    this.staleNote.hidden = !stale;
    this.staleNote.replaceChildren(
      ...(stale
        ? [
            icon("history", "mq-stale-icon"),
            h("span", {}, `These results are from the previous query: the ${field.toLowerCase()} does not parse, so it was not run.`),
          ]
        : []),
    );
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
      this.anchor = null;
      this.userMoved = false;
      this.draw(null);
      this.probeRefs(page.documents as Doc[]);
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

  /** Probes the reference-like fields of a page (bounded, cached per field); the views are drawn again as targets are found. */
  private probeRefs(documents: Doc[]): void {
    for (const [path, sample] of refFields(documents)) {
      if (this.refs.known(path) !== undefined) continue;
      void this.refs.target(path, sample).then((target) => {
        if (target && !this.disposed) this.scheduleRefresh();
      });
    }
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer !== null) return;
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      for (const [mode, renderer] of this.renderers) if (this.shown.get(mode) === this.page) renderer.refresh?.();
    }, 60);
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

  /** Shows the page in the current mode; `anchor`: the document to bring to the top (a mode switch). */
  private draw(anchor: Anchor | null): void {
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
    const fresh = this.shown.get(this.mode) !== page;
    if (fresh) {
      renderer.show(this.shownDocuments(), this.cursors[this.cursors.length - 1]?.offset ?? 0);
      this.shown.set(this.mode, page);
    }
    if (this.results.firstChild !== renderer.element) this.results.replaceChildren(renderer.element);
    if (anchor) {
      const index = indexOfAnchor(documents, anchor);
      renderer.scrollToDocument(index);
      // A mode just attached may lay out later (the JSON editor measures on the next frame).
      requestAnimationFrame(() => {
        if (!this.disposed && this.renderers.get(this.mode) === renderer) renderer.scrollToDocument(index);
      });
    }
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
      renderer = spec.create(this.rendererContext);
      this.renderers.set(mode, renderer);
    }
    return renderer;
  }

  private setMode(mode: DocumentsMode, notify = true): void {
    const current = this.renderers.get(this.mode);
    const documents = (this.page?.documents ?? []) as Doc[];
    // The document at the top of the mode left; when the user did not move in it since it was shown (a page that fits
    // without scrolling cannot reach the anchor), the anchor it was shown at.
    const moved = this.userMoved || !this.anchor;
    const anchor = current && this.page && this.shown.get(this.mode) === this.page ? (moved ? anchorAt(documents, current.topDocument()) : this.anchor) : null;
    this.anchor = anchor;
    this.userMoved = false;
    this.mode = mode;
    for (const [id, button] of this.modeButtons) {
      button.classList.toggle("active", id === mode);
      button.setAttribute("aria-pressed", String(id === mode));
    }
    if (this.page && this.page.documents.length > 0) this.draw(anchor);
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

  /** Opens `collection`'s workspace (or its tab) filtered to the document `id`. */
  private openCollection(collection: string, id: Json): void {
    const kind = this.collectionsOfDatabase().find((table) => table.name === collection)?.kind === "view" ? "view" : "collection";
    const object: ObjectRef = { database: this.database, schema: null, name: collection, kind };
    const request: DocumentsRequest = { filter: `{ _id: ${shellText(id)} }` };
    this.ctx.view.navigator.openWorkspace(this.ctx.view.connection.connectionId, object, "documents", request);
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

interface SidePanelOptions {
  resolve(collection: string, id: Json): Promise<Doc | null>;
  openCollection(collection: string, id: Json): void;
  refs: RendererContext["refs"];
}

/** The panel beside the documents: a referenced document, or an array / document opened from a Table cell. */
class SidePanel {
  readonly element = h("aside", { class: "mq-side", hidden: true, "aria-label": "Details" });
  private readonly title = h("div", { class: "mq-side-title" });
  private readonly subtitle = h("div", { class: "mq-side-sub" });
  private readonly body = h("div", { class: "mq-side-body" });
  private readonly foot = h("div", { class: "mq-side-foot", hidden: true });
  private readonly tree: DocTree;
  private request = 0;

  constructor(private readonly options: SidePanelOptions) {
    this.tree = new DocTree({ numbered: false, refs: options.refs, limits: { topFields: 500, childItems: 200 }, label: "Document" });
    const close = iconButton("x", "Close the panel", () => this.close());
    this.element.append(h("div", { class: "mq-side-head" }, this.title, close), this.subtitle, this.body, this.foot);
    this.element.addEventListener("keydown", (event) => {
      if (event.key === "Escape") this.close();
    });
  }

  inspect(title: string, value: Json): void {
    this.request++;
    this.open(
      h("span", { class: "mq-side-name" }, title),
      Array.isArray(value) ? `${value.length} ${value.length === 1 ? "item" : "items"}` : "Embedded document",
    );
    this.foot.hidden = true;
    this.showTree(value as Doc);
  }

  async openRef(collection: string, id: Json, path: string): Promise<void> {
    const request = ++this.request;
    this.open(
      h("span", { class: "mq-side-name" }, collection),
      `${readable(id).text} · referenced by ${path}`,
      h("span", { class: "dt-inferred", title: "Found by matching the field's name and value, not declared in MongoDB" }, "inferred"),
    );
    const link = h("button", { class: "link-button", type: "button" }, `Open ${collection} filtered by this _id`);
    link.addEventListener("click", () => this.options.openCollection(collection, id));
    this.foot.replaceChildren(link);
    this.foot.hidden = false;
    this.body.replaceChildren(h("div", { class: "mq-side-message" }, "Loading…"));
    try {
      const document = await this.options.resolve(collection, id);
      if (request !== this.request) return;
      if (!document) {
        this.body.replaceChildren(h("div", { class: "mq-side-message" }, `No document with this _id in ${collection} (any more).`));
        return;
      }
      this.showTree(document);
    } catch (error) {
      if (request !== this.request) return;
      this.body.replaceChildren(renderError(error, { context: `Read the document in ${collection}` }));
    }
  }

  dispose(): void {
    this.tree.dispose();
  }

  private open(title: HTMLElement, subtitle: string, badge: HTMLElement | null = null): void {
    this.title.replaceChildren(title, ...(badge ? [badge] : []));
    this.subtitle.textContent = subtitle;
    this.subtitle.title = subtitle;
    this.element.hidden = false;
  }

  private showTree(document: Doc): void {
    this.body.replaceChildren(this.tree.element);
    this.tree.show([document], 0);
  }

  private close(): void {
    this.request++;
    this.element.hidden = true;
  }
}
