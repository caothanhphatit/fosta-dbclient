/**
 * The Explain section of a collection workspace: the query plan (`mongo.explain`) of the Documents query or of the
 * Aggregations pipeline, at the verbosity picked (the plan only, or execution statistics, which run the query within
 * the workspace's timeout). The plan shows as a tree of stage cards, root first, each with its index, keys, filter and
 * counters and a bar of what it returned against what it examined; the winning plan is highlighted and the rejected
 * plans fold under it. Summary badges and warnings (collection scan, in-memory sort, many documents examined per
 * document returned, filtered fields no index serves) sit above it, with the index the query shape asks for by the ESR
 * rule unless one already covers it: "Create index…" opens the Indexes section's form prefilled. Raw JSON shows the
 * server's answer.
 *
 * "Explain" in the Documents query bar and in the Aggregations header hand their query over (`receive`) and run it.
 * The source and the verbosity are kept with the tab; the assistant gets the summary, the warnings and the suggestion.
 */
import { toast } from "../../core/dialogs";
import { copyText, formatNumber, h, icon } from "../../core/dom";
import { errorText } from "../../core/errors";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { shellText } from "./bson";
import { STORE_QUERY } from "./documentsView";
import {
  createIndexRequest,
  examinedOf,
  explainSummary,
  findExplainArgs,
  keyPatternFields,
  keysText,
  pipelineExplainArgs,
  planWarnings,
  readExplainRequest,
  readExplainState,
  returnedShare,
  shapeOfArgs,
  STORE_PIPELINE,
  suggestIndex,
  VERBOSITIES,
  type BuiltExplain,
  type ExplainResult,
  type ExplainSavedState,
  type ExplainSource,
  type IndexSuggestion,
  type PlanNode,
  type PlanWarning,
  type QueryShape,
  type Verbosity,
} from "./explainModel";
import type { IndexesResult } from "./indexModel";
import { STORE_INDEXES } from "./indexesView";
import { MongoApi } from "./mongoApi";
import { EMPTY_QUERY, type QueryState } from "./queryModel";
import "./mongodb.css";
import "./explain.css";

export function explainSectionFactory(ctx: SectionContext): SectionContent {
  return new ExplainSection(ctx);
}

/** One line about what a stage does, shown on hover. */
const STAGE_HINTS: Record<string, string> = {
  COLLSCAN: "Reads every document of the collection",
  IXSCAN: "Walks a range of an index",
  FETCH: "Reads the documents the index entries point to",
  SORT: "Sorts in memory after reading (blocking)",
  SORT_MERGE: "Merges inputs that are already sorted",
  LIMIT: "Stops after a number of documents",
  SKIP: "Skips a number of documents",
  PROJECTION_SIMPLE: "Keeps some fields",
  PROJECTION_DEFAULT: "Shapes the documents returned",
  PROJECTION_COVERED: "Answers from the index alone (covered query)",
  IDHACK: "Looks the document up by _id",
  EXPRESS_IXSCAN: "Looks a single document up by an equality on a unique index",
  SHARDING_FILTER: "Drops documents that belong to other shards (orphans)",
  SHARD_MERGE: "Merges the shards' results",
  SHARD: "One shard's part of the query",
  OR: "Unions the results of several index scans",
  AND_SORTED: "Intersects index scans",
  AND_HASH: "Intersects index scans",
  COUNT_SCAN: "Counts from an index without reading documents",
  DISTINCT_SCAN: "Reads distinct values from an index",
  TEXT_MATCH: "Matches a $text search",
  GROUP: "Groups in the query layer",
  EOF: "The collection does not exist or the query matches nothing by construction",
};

const PREVIEW_CHARS = 180;

function compact(value: unknown): string {
  let text: string;
  try {
    text = typeof value === "string" ? value : shellText(value as never);
  } catch {
    text = JSON.stringify(value);
  }
  return text.length > PREVIEW_CHARS ? `${text.slice(0, PREVIEW_CHARS)}…` : text;
}

function full(value: unknown): string {
  try {
    return typeof value === "string" ? value : shellText(value as never, 2);
  } catch {
    return JSON.stringify(value, null, 2);
  }
}

class ExplainSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly database: string;
  private readonly collection: string;
  private readonly sourceButtons = new Map<ExplainSource, HTMLButtonElement>();
  private readonly verbositySelect: HTMLSelectElement;
  private readonly runButton: HTMLButtonElement;
  private readonly meta = h("span", { class: "ex-meta" });
  private readonly what = h("div", { class: "ex-what mono" });
  private readonly info = h("div", { class: "ex-info", role: "status", hidden: true });
  private readonly viewButtons = new Map<"plan" | "raw", HTMLButtonElement>();
  private readonly planHost = h("div", { class: "ex-plan" });
  private readonly rawHost = h("div", { class: "ex-raw", hidden: true });
  private state: ExplainSavedState;
  /** The query handed over by Documents' Explain (as typed then); dropped when Documents runs another query. */
  private handedQuery: QueryState | null = null;
  private result: ExplainResult | null = null;
  private resultText = "";
  private shape: QueryShape | null = null;
  private warnings: PlanWarning[] = [];
  private suggestion: IndexSuggestion | null = null;
  private lastError: string | null = null;
  private generation = 0;
  private disposed = false;
  private readonly unsubscribe: (() => void)[] = [];

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    this.state = readExplainState(ctx.savedState);

    const sources = h("div", { class: "segmented ex-sources", role: "group", "aria-label": "Explain" });
    for (const [source, label, title] of [
      ["query", "Documents query", "The query of the Documents section (the last one run, or the one handed over by its Explain)"],
      ["pipeline", "Pipeline", "The enabled stages of the Aggregations section"],
    ] as const) {
      const button = h("button", { class: "segment", type: "button", title, "aria-pressed": "false" }, label);
      button.addEventListener("click", () => {
        if (this.state.source === source) return;
        this.state.source = source;
        this.ctx.stateChanged();
        this.drawSource();
      });
      this.sourceButtons.set(source, button);
      sources.append(button);
    }
    this.verbositySelect = h("select", { class: "input ex-verbosity", "aria-label": "Verbosity" });
    for (const item of VERBOSITIES) {
      const option = h("option", { value: item.id, title: item.title }, item.label);
      option.selected = item.id === this.state.verbosity;
      this.verbositySelect.append(option);
    }
    this.verbositySelect.addEventListener("change", () => {
      this.state.verbosity = this.verbositySelect.value as Verbosity;
      this.verbositySelect.title = VERBOSITIES.find((item) => item.id === this.state.verbosity)?.title ?? "";
      this.ctx.stateChanged();
    });
    this.verbositySelect.title = VERBOSITIES.find((item) => item.id === this.state.verbosity)?.title ?? "";
    this.runButton = h("button", { class: "btn primary", type: "button", title: "Explain the query" }, icon("bolt"), h("span", {}, "Explain"));
    this.runButton.addEventListener("click", () => void this.run());

    const views = h("div", { class: "segmented ex-views", role: "group", "aria-label": "Show" });
    for (const [view, label] of [
      ["plan", "Plan"],
      ["raw", "Raw JSON"],
    ] as const) {
      const button = h("button", { class: "segment", type: "button", "aria-pressed": String(view === "plan") }, label);
      button.addEventListener("click", () => this.showView(view));
      this.viewButtons.set(view, button);
      views.append(button);
    }
    this.viewButtons.get("plan")?.classList.add("active");

    const bar = h(
      "div",
      { class: "ex-bar" },
      sources,
      h("label", { class: "ex-bar-field" }, h("span", { class: "ex-label" }, "Verbosity"), this.verbositySelect),
      this.runButton,
      h("div", { class: "toolbar-end" }, this.meta, views),
    );
    this.element = h("div", { class: "view ex-section" }, bar, this.what, this.info, h("div", { class: "ex-scroll" }, this.planHost, this.rawHost));
    this.unsubscribe.push(
      ctx.store.subscribe(STORE_QUERY, () => {
        this.handedQuery = null;
        this.drawSource();
      }),
      ctx.store.subscribe<IndexesResult>(STORE_INDEXES, () => {
        if (this.result) this.analyse();
      }),
    );
    this.drawSource();
    this.drawPlan();
  }

  dispose(): void {
    this.disposed = true;
    for (const stop of this.unsubscribe) stop();
  }

  saveState(): unknown {
    return { ...this.state };
  }

  assistantContext(): string | null {
    const lines: string[] = [];
    if (this.result) lines.push(explainSummary(this.result, this.resultText, this.warnings, this.suggestion));
    else lines.push(`Explain of ${this.database}.${this.collection}: not run yet (source: ${this.state.source === "query" ? "Documents query" : "pipeline"}).`);
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  /** `{ explain: { source, query?, stages? } }` from Documents or Aggregations: shows that source and runs it. */
  receive(request: unknown): void {
    const explain = readExplainRequest(request);
    if (!explain) return;
    this.state.source = explain.source;
    if (explain.source === "query" && explain.query) this.handedQuery = explain.query;
    this.ctx.stateChanged();
    this.drawSource();
    void this.run(explain.stages);
  }

  // --- Source ----------------------------------------------------------------------------------------------------------

  private query(): QueryState | null {
    return this.handedQuery ?? this.ctx.store.get<QueryState>(STORE_QUERY) ?? null;
  }

  private stages(): string[] | null {
    const read = this.ctx.store.get<() => string[]>(STORE_PIPELINE);
    return typeof read === "function" ? read() : null;
  }

  private build(stages?: string[]): BuiltExplain {
    const target = { database: this.database, collection: this.collection };
    if (this.state.source === "query") return findExplainArgs(this.query() ?? EMPTY_QUERY, target, this.state.verbosity);
    const texts = stages ?? this.stages();
    if (!texts) return { ok: false, error: "Open the Aggregations section and build a pipeline first, or press Explain in its header." };
    return pipelineExplainArgs(texts, target, this.state.verbosity);
  }

  private drawSource(): void {
    for (const [source, button] of this.sourceButtons) {
      const on = source === this.state.source;
      button.classList.toggle("active", on);
      button.setAttribute("aria-pressed", String(on));
    }
    const built = this.build();
    const none = this.state.source === "query" && !this.query();
    this.what.textContent = built.ok
      ? `db.getCollection(${JSON.stringify(this.collection)}).${built.text}${none ? "   (no Documents query run yet: the whole collection)" : ""}`
      : built.error;
    this.what.classList.toggle("ex-what-error", !built.ok);
  }

  private showView(view: "plan" | "raw"): void {
    for (const [id, button] of this.viewButtons) {
      button.classList.toggle("active", id === view);
      button.setAttribute("aria-pressed", String(id === view));
    }
    this.planHost.hidden = view !== "plan";
    this.rawHost.hidden = view !== "raw";
  }

  private showInfo(message: string | null, tone: "info" | "error" = "info"): void {
    this.info.hidden = !message;
    this.info.classList.toggle("ex-info-error", tone === "error");
    this.info.replaceChildren(...(message ? [icon(tone === "error" ? "x" : "health", "ex-info-icon"), h("span", {}, message)] : []));
  }

  // --- Running ---------------------------------------------------------------------------------------------------------

  private async run(stages?: string[]): Promise<void> {
    const built = this.build(stages);
    if (!built.ok) {
      this.showInfo(built.error, "error");
      this.lastError = built.error;
      return;
    }
    const generation = ++this.generation;
    this.runButton.disabled = true;
    this.showInfo(this.state.verbosity === "queryPlanner" ? "Asking the planner…" : "Running the query to count what each stage does…");
    try {
      const result = await this.api.explain(built.args);
      if (this.disposed || generation !== this.generation) return;
      if (!result) {
        this.showInfo("The explain was not run (the confirmation was declined).");
        return;
      }
      this.result = result;
      this.resultText = built.text;
      this.shape = shapeOfArgs(built.args);
      this.lastError = null;
      this.showInfo(null);
      this.analyse();
      void this.loadIndexes();
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
    } finally {
      if (generation === this.generation && !this.disposed) {
        this.runButton.disabled = false;
        this.drawMeta();
      }
    }
  }

  /** The indexes for the suggestion: the Indexes section's, else read once without asking. */
  private async loadIndexes(): Promise<void> {
    if (this.ctx.store.get(STORE_INDEXES)) return;
    try {
      const result = await this.api.indexes(false);
      if (result && !this.disposed) this.ctx.store.set(STORE_INDEXES, result);
    } catch {
      // The suggestion shows without the coverage check.
    }
  }

  private analyse(): void {
    const result = this.result;
    if (!result) return;
    this.warnings = planWarnings(result.plan, this.shape, formatNumber);
    const indexes = this.ctx.store.get<IndexesResult>(STORE_INDEXES)?.indexes ?? null;
    const flagged = this.warnings.some((warning) => warning.tone === "warn");
    this.suggestion = this.shape && (flagged || !result.plan) ? suggestIndex(this.shape, indexes) : null;
    this.drawPlan();
    this.drawRaw();
  }

  private drawMeta(): void {
    const result = this.result;
    this.meta.textContent = result
      ? `${VERBOSITIES.find((item) => item.id === result.verbosity)?.label ?? result.verbosity} · ${formatNumber(result.elapsedMs)} ms`
      : "";
  }

  // --- The plan --------------------------------------------------------------------------------------------------------

  private drawPlan(): void {
    const result = this.result;
    if (!result) {
      this.planHost.replaceChildren(
        h(
          "div",
          { class: "ex-empty" },
          h("p", {}, "Pick the Documents query or the pipeline, then press Explain."),
          h(
            "p",
            { class: "ex-dim" },
            "Query planner shows the chosen plan; Execution stats also runs the query (within the workspace's timeout) and counts what each stage read.",
          ),
        ),
      );
      return;
    }
    const parts: HTMLElement[] = [];
    const plan = result.plan;
    if (plan) parts.push(this.summaryRow(plan.summary, plan.shape));
    if (this.warnings.length) parts.push(h("ul", { class: "ex-warnings" }, ...this.warnings.map((warning) => this.warningItem(warning))));
    const suggestion = this.suggestionCard();
    if (suggestion) parts.push(suggestion);
    if (plan) {
      parts.push(
        h("h3", { class: "ex-heading" }, "Winning plan", h("span", { class: "ex-dim" }, " — root first; each stage reads from the ones under it")),
        h("ul", { class: "ex-tree ex-winning" }, this.nodeItem(plan.winningPlan, true)),
      );
      if (plan.rejectedPlans.length) {
        const rejected = h(
          "details",
          { class: "ex-rejected" },
          h("summary", {}, `Rejected plans (${plan.rejectedPlans.length})`),
          ...plan.rejectedPlans.map((entry, index) =>
            h(
              "div",
              { class: "ex-rejected-plan" },
              h("div", { class: "ex-dim" }, `Plan ${index + 2}${entry.shard ? ` on ${entry.shard}` : ""}`),
              h("ul", { class: "ex-tree" }, this.nodeItem(entry.plan, false)),
            ),
          ),
        );
        parts.push(rejected);
      }
    }
    this.planHost.replaceChildren(...parts);
  }

  private summaryRow(summary: NonNullable<ExplainResult["plan"]>["summary"], shape: string): HTMLElement {
    const badge = (label: string, value: string, tone = "", title = "") =>
      h("span", { class: `ex-badge${tone ? ` ex-badge-${tone}` : ""}`, title }, h("span", { class: "ex-badge-label" }, label), h("strong", {}, value));
    const number = (value: number | null) => (value === null ? "—" : formatNumber(value));
    const indexes = summary.usedIndexes.length ? summary.usedIndexes.join(", ") : "none";
    return h(
      "div",
      { class: "ex-summary" },
      badge("Returned", number(summary.nReturned)),
      badge("Keys examined", number(summary.keysExamined)),
      badge("Docs examined", number(summary.docsExamined), summary.fetchRatio !== null && summary.fetchRatio >= 10 ? "warn" : ""),
      badge("Time", summary.executionTimeMillis === null ? "—" : `${formatNumber(summary.executionTimeMillis)} ms`),
      badge("Index", indexes, summary.usedIndexes.length ? "good" : "warn"),
      summary.isCollScan ? badge("Scan", "COLLSCAN", "warn", "Reads every document") : null,
      summary.sortInMemory ? badge("Sort", "in memory", "warn", "A blocking SORT stage") : null,
      summary.fetchRatio !== null ? badge("Examined / returned", `${summary.fetchRatio}`, summary.fetchRatio >= 10 ? "warn" : "") : null,
      shape === "sharded" ? badge("Cluster", "sharded") : null,
      summary.nReturned === null ? h("span", { class: "ex-dim" }, "Counters need Execution stats.") : null,
    );
  }

  private warningItem(warning: PlanWarning): HTMLElement {
    return h(
      "li",
      { class: `ex-warning ex-warning-${warning.tone}`, "data-warning": warning.id },
      icon(warning.tone === "warn" ? "bolt" : "health", "ex-warning-icon"),
      h("div", {}, h("strong", {}, warning.title), h("div", { class: "ex-warning-detail" }, warning.detail)),
    );
  }

  private suggestionCard(): HTMLElement | null {
    const suggestion = this.suggestion;
    if (!suggestion) return null;
    if (suggestion.coveredBy) {
      return h(
        "div",
        { class: "ex-suggestion ex-suggestion-covered" },
        icon("key", "ex-suggestion-icon"),
        h(
          "div",
          {},
          h("strong", {}, `Index ${suggestion.coveredBy} covers this filter and sort`),
          h("div", { class: "ex-dim" }, "No new index is suggested; if the plan does not use it, compare the rejected plans or hint it."),
        ),
      );
    }
    const create = h("button", { class: "btn primary", type: "button" }, icon("plus"), h("span", {}, "Create index…"));
    create.addEventListener("click", () => void this.ctx.deliver("indexes", createIndexRequest(suggestion)));
    const copy = h("button", { class: "btn", type: "button", title: "Copy the mongosh createIndex" }, icon("copy"), h("span", {}, "Copy"));
    copy.addEventListener("click", () => {
      void copyText(`db.getCollection(${JSON.stringify(this.collection)}).createIndex(${keysText(suggestion.keys)})`).then(() =>
        toast("Copied the createIndex command", "success"),
      );
    });
    return h(
      "div",
      { class: "ex-suggestion" },
      icon("key", "ex-suggestion-icon"),
      h(
        "div",
        { class: "ex-suggestion-body" },
        h("strong", {}, "Suggested index"),
        h(
          "div",
          { class: "ex-suggestion-keys" },
          ...suggestion.keys.map((key) =>
            h(
              "span",
              { class: "ex-key" },
              h("span", {}, key.field),
              h("span", { class: `ex-kind ex-kind-${key.kind > 0 ? "asc" : "desc"}` }, key.kind > 0 ? "↑ 1" : "↓ -1"),
            ),
          ),
        ),
        h("div", { class: "ex-dim" }, suggestion.reason),
        suggestion.hidden
          ? h("div", { class: "ex-dim" }, `The hidden index ${suggestion.hidden} has these keys: unhiding it (Indexes) is cheaper than building another.`)
          : null,
      ),
      h("div", { class: "ex-suggestion-actions" }, copy, create),
    );
  }

  private nodeItem(node: PlanNode, winning: boolean): HTMLElement {
    const stage = node.stage ?? "?";
    const head = h(
      "div",
      { class: "ex-card-head" },
      h("span", { class: `ex-stage ex-stage-${stage.toLowerCase().replace(/[^a-z_]/g, "")}`, title: STAGE_HINTS[stage] ?? "" }, stage),
      node.indexName ? h("span", { class: "ex-index", title: "Index" }, node.indexName) : null,
      node.keyPattern
        ? h("span", { class: "ex-keys mono", title: full(node.keyPattern) }, keyPatternFields(node.keyPattern).length ? compact(node.keyPattern) : "")
        : null,
      node.direction && node.direction !== "forward" ? h("span", { class: "ex-dim" }, node.direction) : null,
      node.isMultiKey ? h("span", { class: "ex-tag", title: "The index has an array field" }, "multikey") : null,
      node.shard ? h("span", { class: "ex-tag" }, node.shard) : null,
    );
    const metrics: string[] = [];
    if (typeof node.nReturned === "number") metrics.push(`${formatNumber(node.nReturned)} returned`);
    if (typeof node.totalKeysExamined === "number") metrics.push(`${formatNumber(node.totalKeysExamined)} keys`);
    if (typeof node.totalDocsExamined === "number") metrics.push(`${formatNumber(node.totalDocsExamined)} docs`);
    if (typeof node.executionTimeMillisEstimate === "number") metrics.push(`~${formatNumber(node.executionTimeMillisEstimate)} ms`);
    const share = returnedShare(node);
    const examined = examinedOf(node);
    const bar =
      share !== null && examined !== null && examined > 0
        ? h(
            "div",
            {
              class: `ex-meter${share < 0.1 ? " ex-meter-low" : ""}`,
              title: `${formatNumber(node.nReturned ?? 0)} returned of ${formatNumber(examined)} examined`,
            },
            h("span", { class: "ex-meter-fill", style: `width: ${Math.max(1, Math.round(share * 100))}%` }),
          )
        : null;
    const detail = (label: string, value: unknown) =>
      value === undefined || value === null
        ? null
        : h("div", { class: "ex-detail mono", title: full(value) }, h("span", { class: "ex-dim" }, `${label} `), compact(value));
    const card = h(
      "div",
      { class: `ex-card${winning ? " ex-card-winning" : ""}${stage === "COLLSCAN" || stage === "SORT" ? " ex-card-warn" : ""}` },
      head,
      metrics.length ? h("div", { class: "ex-metrics" }, metrics.join(" · ")) : null,
      bar,
      detail("filter", node.filter),
      detail("bounds", node.bounds),
      detail(stage.startsWith("$") ? "spec" : "details", node.details),
    );
    const children = node.children ?? [];
    return h(
      "li",
      { class: "ex-node" },
      card,
      children.length ? h("ul", { class: "ex-children" }, ...children.map((child) => this.nodeItem(child, winning))) : null,
    );
  }

  private drawRaw(): void {
    const result = this.result;
    if (!result) {
      this.rawHost.replaceChildren();
      return;
    }
    const text = JSON.stringify(result.explain, null, 2);
    const copy = h("button", { class: "btn", type: "button" }, icon("copy"), h("span", {}, "Copy JSON"));
    copy.addEventListener("click", () => void copyText(text).then(() => toast("Copied the explain output", "success")));
    const copyStatement = h("button", { class: "btn", type: "button", title: result.statement }, icon("terminal"), h("span", {}, "Copy mongosh"));
    copyStatement.addEventListener("click", () => void copyText(result.statement).then(() => toast("Copied the mongosh command", "success")));
    this.rawHost.replaceChildren(
      h("div", { class: "ex-raw-bar" }, h("code", { class: "ex-statement mono" }, result.statement), copyStatement, copy),
      h("pre", { class: "ex-json mono" }, text),
    );
  }
}
