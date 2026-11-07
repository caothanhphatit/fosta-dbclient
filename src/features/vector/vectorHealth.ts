import { api, errorText } from "../../core/api";
import { toast } from "../../core/dialogs";
import { formatNumber, h, separator, spacer, toolButton } from "../../core/dom";
import type { TabContent } from "../../core/tabs";
import type { CheckStatus, HealthCheck, RecallReport, TableTarget } from "../../core/types";

const STATUS_MARK: Record<CheckStatus, string> = { ok: "✓", info: "i", warn: "!", error: "✕" };
const DEFAULT_SAMPLE = 2000;
const DEFAULT_RECALL_K = 10;
const DEFAULT_RECALL_QUERIES = 20;

export class VectorHealthView implements TabContent {
  readonly element: HTMLElement;
  private readonly target: TableTarget;
  private readonly columnSelect = h("select", { class: "input" });
  private readonly sampleInput = h("input", { class: "input k-input", type: "number", min: 1, max: 50000, value: String(DEFAULT_SAMPLE) });
  private readonly kInput = h("input", { class: "input k-input", type: "number", min: 1, max: 100, value: String(DEFAULT_RECALL_K) });
  private readonly queriesInput = h("input", { class: "input k-input", type: "number", min: 1, max: 100, value: String(DEFAULT_RECALL_QUERIES) });
  private readonly checks = h("div", { class: "health-checks" });
  private readonly recall = h("div", { class: "recall-report" });

  constructor(target: TableTarget, column: string) {
    this.target = target;
    const toolbar = h(
      "div",
      { class: "toolbar" },
      toolButton("health", "Run checks", () => void this.runChecks()),
      labelled("Column", this.columnSelect),
      labelled("Sample", this.sampleInput),
      separator(),
      toolButton("vector", "Measure recall", () => void this.measureRecall(), "Compare index results with an exact full scan"),
      labelled("k", this.kInput),
      labelled("Queries", this.queriesInput),
      spacer(),
      h("span", { class: "toolbar-title" }, `${target.database}.${target.table}`),
    );
    const recallIntro = h(
      "p",
      { class: "muted-text" },
      "Recall compares the vector index (approximate) with an exact full scan for random rows from the table. 1.00 means the index returns exactly the true nearest neighbours. Exact queries scan the whole table, so this can take a while on large tables.",
    );
    this.element = h(
      "div",
      { class: "view vector-health" },
      toolbar,
      h("div", { class: "health-body" }, h("div", { class: "section-title" }, "Data checks"), this.checks, h("div", { class: "section-title" }, "Index recall"), recallIntro, this.recall),
    );
    void this.loadColumns(column);
  }

  private async loadColumns(preferred: string): Promise<void> {
    try {
      const info = await api.vectorInfo(this.target.connectionId, this.target.database, this.target.table);
      this.columnSelect.replaceChildren(
        ...info.columns.map((column) => h("option", { value: column.name, selected: column.name === preferred }, `${column.name} (${column.dims})`)),
      );
      await this.runChecks();
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  private async runChecks(): Promise<void> {
    this.checks.replaceChildren(h("div", { class: "empty-hint" }, "Checking…"));
    try {
      const checks = await api.vectorHealth(
        this.target.connectionId,
        this.target.database,
        this.target.table,
        this.columnSelect.value,
        Number(this.sampleInput.value) || DEFAULT_SAMPLE,
      );
      this.checks.replaceChildren(...checks.map(checkCard));
    } catch (error) {
      this.checks.replaceChildren(checkCard({ label: "Checks", status: "error", value: "failed", detail: errorText(error) }));
    }
  }

  private async measureRecall(): Promise<void> {
    this.recall.replaceChildren(h("div", { class: "empty-hint" }, "Running approximate and exact searches…"));
    try {
      const report = await api.indexRecall(
        this.target.connectionId,
        this.target.database,
        this.target.table,
        this.columnSelect.value,
        Number(this.kInput.value) || DEFAULT_RECALL_K,
        Number(this.queriesInput.value) || DEFAULT_RECALL_QUERIES,
      );
      this.recall.replaceChildren(recallSummary(report));
    } catch (error) {
      this.recall.replaceChildren(checkCard({ label: "Recall", status: "error", value: "failed", detail: errorText(error) }));
    }
  }
}

function checkCard(check: HealthCheck): HTMLElement {
  return h(
    "div",
    { class: `health-card ${check.status}` },
    h("span", { class: "health-mark" }, STATUS_MARK[check.status]),
    h("div", { class: "health-text" }, h("div", { class: "health-label" }, check.label), check.detail ? h("div", { class: "health-detail" }, check.detail) : null),
    h("div", { class: "health-value mono" }, check.value),
  );
}

function recallSummary(report: RecallReport): HTMLElement {
  const status: CheckStatus = report.recall >= 0.95 ? "ok" : report.recall >= 0.8 ? "warn" : "error";
  const speedup = report.annMs > 0 ? report.exactMs / report.annMs : 0;
  return h(
    "div",
    { class: `recall-card ${status}` },
    h("div", { class: "recall-big" }, `Recall@${report.k}: ${report.recall.toFixed(3)}`),
    h(
      "div",
      { class: "recall-grid" },
      metric("Worst query", report.worstRecall.toFixed(3)),
      metric("Queries", formatNumber(report.queries)),
      metric("Index search", `${report.annMs.toFixed(1)} ms`),
      metric("Exact search", `${report.exactMs.toFixed(1)} ms`),
      metric("Speed-up", speedup ? `×${speedup.toFixed(1)}` : "n/a"),
      metric("Index", `${report.index} · ${report.metric}`),
    ),
    status === "ok" ? null : h("p", { class: "muted-text" }, "Low recall: consider a larger M when creating the vector index, or raise mhnsw_ef_search for the session."),
  );
}

function metric(label: string, value: string): HTMLElement {
  return h("div", { class: "metric" }, h("div", { class: "metric-label" }, label), h("div", { class: "metric-value" }, value));
}

function labelled(label: string, control: HTMLElement): HTMLElement {
  return h("label", { class: "inline-field" }, h("span", {}, label), control);
}
