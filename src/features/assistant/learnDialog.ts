/**
 * "Learn this database": one dialog that explains what happens, builds the profile with a step list and Cancel, then
 * offers the model's summary with its estimated cost (Summarise / Skip) and ends with a short result. The build sends
 * nothing to a model; only the summary step does, and only when the user picks Summarise.
 */
import { api, errorText, onProfileProgress } from "../../core/api";
import { checkbox, openModal } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import type { DatabaseProfile, ProfileProgress, ProfileStep, SummaryCost, SummaryOutcome } from "../../core/profileTypes";
import type { AppConfig } from "../../core/types";
import { connectionSettings, providerPreset } from "./aiProviders";
import { formatTokens, LEARN_STEPS, learningAllowed, learningRefusal, readsAllowed, SUMMARY_STEP_LABEL } from "./profileLogic";

export interface LearnTarget {
  connectionId: string;
  connectionName: string;
  /** Empty for engines without databases. */
  database: string;
}

export interface LearnOptions {
  /** A refresh of an existing profile: the summary step then describes only objects without a current description. */
  onlyChanged?: boolean;
  /** Shown on the result step: opens "What the assistant knows". */
  openKnowledge?: () => void;
}

/** What the dialog did: whether a profile was built (or refreshed). */
export type LearnResult = { learned: boolean };

type StepState = "pending" | "running" | "done" | "skipped" | "failed";

/** Runs the whole flow; resolves when the dialog closes. */
export function learnDialog(config: AppConfig | null, target: LearnTarget, options: LearnOptions = {}): Promise<LearnResult> {
  return new Promise((resolve) => new LearnFlow(config, target, options, resolve).start());
}

function shownName(target: LearnTarget): string {
  return target.database || target.connectionName;
}

class LearnFlow {
  private readonly body = h("div", { class: "learn-dialog" });
  private footer: HTMLElement | null = null;
  private close: () => void = () => undefined;
  private runId: string | null = null;
  private learned = false;
  private closed = false;

  constructor(
    private readonly config: AppConfig | null,
    private readonly target: LearnTarget,
    private readonly options: LearnOptions,
    private readonly resolve: (result: LearnResult) => void,
  ) {}

  start(): void {
    const title = this.options.onlyChanged ? `Refresh what the assistant knows about ${shownName(this.target)}` : `Learn ${shownName(this.target)}`;
    this.close = openModal(title, this.body, [], {
      width: 540,
      onClose: () => {
        this.closed = true;
        // Escape while a step runs stops it; what was answered so far is kept.
        if (this.runId) void api.aiCancel(this.runId);
        this.resolve({ learned: this.learned });
      },
    });
    this.footer = this.body.closest(".modal")?.querySelector<HTMLElement>(".modal-footer") ?? null;
    this.intro();
  }

  private buttons(...buttons: HTMLButtonElement[]): void {
    this.footer?.replaceChildren(...buttons);
  }

  private button(label: string, run: () => void, kind: "primary" | "plain" | "left" = "plain"): HTMLButtonElement {
    const classes = ["btn", kind === "primary" ? "primary" : "", kind === "left" ? "align-left" : ""].filter(Boolean).join(" ");
    const button = h("button", { class: classes, type: "button" }, label);
    button.addEventListener("click", run);
    return button;
  }

  // --- 1. What will happen ---------------------------------------------------------------------------------------------

  private intro(): void {
    const { connectionId } = this.target;
    if (!learningAllowed(this.config, connectionId)) {
      this.body.replaceChildren(h("p", { class: "learn-text" }, learningRefusal(this.config, connectionId)));
      this.buttons(this.button("Close", () => this.close(), "primary"));
      return;
    }
    const reads = readsAllowed(this.config, connectionId);
    const sample = checkbox("Sample column values (enum lists and formats)", reads);
    sample.input.disabled = !reads;
    const sampleHint = reads
      ? "Reads at most a few distinct values per short text column, as the assistant, through the workspace rules. Passwords, tokens and similar columns are never sampled."
      : "Off: the assistant may not read data on this connection (Settings → Allow the assistant to run read-only queries, or Edit workspace → Assistant queries).";
    this.body.replaceChildren(
      h(
        "p",
        { class: "learn-text" },
        `Fosta Studio reads the structure of ${shownName(this.target)}: tables, views and collections, their columns, keys, indexes and relations. It saves it on this computer so the assistant knows the database in every chat, and tells you when the schema changes.`,
      ),
      h(
        "ul",
        { class: "learn-points" },
        h("li", {}, "Nothing is sent to your AI provider while learning."),
        h("li", {}, "Afterwards you can let the model write a summary and short descriptions; you see the estimated cost first."),
      ),
      h("div", { class: "learn-option" }, sample.element, h("span", { class: "field-hint" }, sampleHint)),
    );
    this.buttons(
      this.button("Cancel", () => this.close()),
      this.button(this.options.onlyChanged ? "Refresh" : "Learn", () => void this.build(reads && sample.input.checked), "primary"),
    );
  }

  // --- 2. Building, step by step ---------------------------------------------------------------------------------------

  private async build(sampleValues: boolean): Promise<void> {
    const steps = LEARN_STEPS.filter((item) => item.step !== "values" || sampleValues);
    const rows = new Map<ProfileStep, { row: HTMLElement; count: HTMLElement; detail: HTMLElement }>();
    const list = h("ol", { class: "learn-steps" });
    for (const item of steps) {
      const count = h("span", { class: "learn-step-count" });
      const detail = h("span", { class: "learn-step-detail" });
      const row = h(
        "li",
        { class: "learn-step pending" },
        h("span", { class: "learn-step-mark" }),
        h("span", { class: "learn-step-label" }, item.label),
        count,
        detail,
      );
      rows.set(item.step, { row, count, detail });
      list.append(row);
    }
    const setState = (step: ProfileStep, state: StepState) => {
      const entry = rows.get(step);
      if (!entry) return;
      entry.row.className = `learn-step ${state}`;
      entry.row.querySelector(".learn-step-mark")?.replaceChildren(...stepMark(state));
      if (state !== "running") entry.detail.textContent = "";
    };
    const order = steps.map((item) => item.step);
    let current = -1;
    const progress = (event: ProfileProgress) => {
      const index = order.indexOf(event.step);
      if (index < 0) return;
      for (let earlier = Math.max(current, 0); earlier < index; earlier += 1) setState(order[earlier], "done");
      current = Math.max(current, index);
      setState(event.step, "running");
      const entry = rows.get(event.step)!;
      entry.count.textContent = event.total > 0 ? `${event.done} / ${event.total}` : event.done > 0 ? String(event.done) : "";
      entry.detail.textContent = event.object ?? "";
    };
    this.body.replaceChildren(h("p", { class: "learn-text" }, `Learning ${shownName(this.target)}…`), list);
    const runId = crypto.randomUUID();
    this.runId = runId;
    this.buttons(this.button("Cancel", () => void api.aiCancel(runId)));
    const unlisten = await onProfileProgress((event) => {
      if (event.runId === runId) progress(event);
    });
    try {
      const outcome = await api.profileLearn(this.target.connectionId, this.target.database, runId, sampleValues);
      for (const step of order) setState(step, "done");
      this.learned = true;
      this.runId = null;
      if (this.closed) return;
      const note = outcome.values?.skipped ? `Value sampling stopped early: ${outcome.values.skipped}` : null;
      await this.offerSummary(note);
    } catch (error) {
      this.runId = null;
      if (current >= 0) setState(order[current], "failed");
      if (this.closed) return;
      const message = errorText(error);
      this.body.append(h("p", { class: "learn-error" }, message === "Stopped" ? "Stopped. Nothing was saved." : message));
      this.buttons(this.button("Close", () => this.close(), "primary"));
    } finally {
      unlisten();
    }
  }

  // --- 3. The model's summary: cost first ------------------------------------------------------------------------------

  private async offerSummary(note: string | null): Promise<void> {
    const config = this.config;
    const connection = config?.aiConnections.find((item) => item.id === config.aiActive?.connectionId);
    if (!config?.aiActive || !connection) {
      await this.finish(null, [note, "Add an LLM provider (Assistant → Set up…) to have the model summarise the database later."]);
      return;
    }
    const activeModel = config.aiActive.model || connection.defaultModel;
    const picker = h("select", { class: "input learn-model" }, h("option", { value: activeModel }, activeModel));
    const costHost = h("div", { class: "learn-cost" }, h("span", { class: "learn-step-detail" }, "Estimating…"));
    const summarise = this.button("Summarise", () => void this.summarise(picker.value), "primary");
    summarise.disabled = true;
    let estimateSeq = 0;
    const estimate = async () => {
      const seq = (estimateSeq += 1);
      summarise.disabled = true;
      try {
        const cost = await api.profileSummaryEstimate(this.target.connectionId, this.target.database, picker.value, this.options.onlyChanged ?? false);
        if (seq !== estimateSeq) return;
        costHost.replaceChildren(...costRows(cost));
        summarise.disabled = false;
      } catch (error) {
        if (seq === estimateSeq) costHost.replaceChildren(h("p", { class: "learn-error" }, errorText(error)));
      }
    };
    picker.addEventListener("change", () => void estimate());
    this.body.replaceChildren(
      h(
        "p",
        { class: "learn-text" },
        `The structure of ${shownName(this.target)} is saved. The model can now write a short summary of the database and one line per object; this sends the structure (and your notes) to your AI provider.`,
      ),
      ...(note ? [h("p", { class: "learn-note" }, note)] : []),
      h("label", { class: "field" }, h("span", { class: "field-label" }, "Model"), picker),
      costHost,
    );
    this.buttons(
      this.button("Skip (structure only)", () => void this.finish(null, [note]), "left"),
      summarise,
    );
    void estimate();
    // The provider's other models, loaded with its saved key; the active one stays the default.
    void api
      .listAiModels(connectionSettings(connection), null, connection.id)
      .then((models) => {
        for (const model of [connection.defaultModel, ...models]) {
          if (model && ![...picker.options].some((option) => option.value === model)) picker.append(h("option", { value: model }, model));
        }
      })
      .catch(() => undefined);
  }

  private async summarise(model: string): Promise<void> {
    const runId = crypto.randomUUID();
    this.runId = runId;
    const count = h("span", { class: "learn-step-count" });
    const row = h(
      "li",
      { class: "learn-step running" },
      h("span", { class: "learn-step-mark" }, ...stepMark("running")),
      h("span", { class: "learn-step-label" }, SUMMARY_STEP_LABEL),
      count,
    );
    this.body.replaceChildren(h("p", { class: "learn-text" }, `Asking ${model}…`), h("ol", { class: "learn-steps" }, row));
    this.buttons(this.button("Cancel", () => void api.aiCancel(runId)));
    const unlisten = await onProfileProgress((event) => {
      if (event.runId === runId && event.total > 0) count.textContent = `${event.done} / ${event.total}`;
    });
    try {
      const outcome = await api.profileSummarize(this.target.connectionId, this.target.database, runId, model, this.options.onlyChanged ?? false);
      this.runId = null;
      if (!this.closed) await this.finish(outcome, []);
    } catch (error) {
      this.runId = null;
      if (this.closed) return;
      const message = errorText(error);
      const stopped = message === "Stopped";
      await this.finish(null, [stopped ? "Stopped. The descriptions written before you stopped are kept." : `The summary failed: ${message}`]);
    } finally {
      unlisten();
    }
  }

  // --- 4. Result -------------------------------------------------------------------------------------------------------

  private async finish(summary: SummaryOutcome | null, notes: (string | null)[]): Promise<void> {
    let profile: DatabaseProfile | null = null;
    try {
      profile = await api.profileGet(this.target.connectionId, this.target.database);
    } catch {
      // The counts below fall back to what the steps reported.
    }
    if (this.closed) return;
    const hints = profile?.objects.reduce((sum, object) => sum + (object.valueHints?.length ?? 0), 0) ?? 0;
    const described = profile?.objects.filter((object) => object.description).length ?? summary?.described ?? 0;
    const facts: [string, string][] = [
      ["Objects", String(profile?.totalObjects ?? 0)],
      ["Relations", String(profile?.relations.length ?? 0)],
      ["Value hints", profile?.dataRead ? String(hints) : "not sampled"],
      ["Described", String(described)],
    ];
    if (summary) facts.push(["Tokens", `~${formatTokens(summary.inputTokens)} in, ~${formatTokens(summary.outputTokens)} out (${summary.model})`]);
    this.body.replaceChildren(
      h("p", { class: "learn-text learn-done" }, icon("check"), `The assistant now knows ${shownName(this.target)}.`),
      h("dl", { class: "learn-facts" }, ...facts.flatMap(([label, value]) => [h("dt", {}, label), h("dd", {}, value)])),
      ...notes.filter((text): text is string => Boolean(text)).map((text) => h("p", { class: "learn-note" }, text)),
    );
    const done = this.button("Done", () => this.close(), "primary");
    const open = this.options.openKnowledge;
    this.buttons(
      ...(open
        ? [
            this.button(
              "Open what the assistant knows",
              () => {
                this.close();
                open();
              },
              "left",
            ),
          ]
        : []),
      done,
    );
    done.focus();
  }
}

function stepMark(state: StepState): HTMLElement[] {
  if (state === "running") return [h("span", { class: "chat-run-spinner" })];
  if (state === "done") return [icon("check")];
  if (state === "failed") return [icon("x")];
  return [h("span", { class: "learn-step-dot" })];
}

function costRows(cost: SummaryCost): HTMLElement[] {
  const facts: [string, string][] = [
    ["Objects to describe", String(cost.objects)],
    ["Requests", String(cost.requests)],
    ["Tokens", `~${formatTokens(cost.inputTokens)} in, ~${formatTokens(cost.outputTokens)} out`],
    ["Model", cost.model],
    ["Provider", providerPreset(cost.provider).label],
  ];
  if (cost.price !== null) facts.push(["Price", `about $${cost.price.toFixed(cost.price < 0.1 ? 3 : 2)}`]);
  return [
    h("dl", { class: "learn-facts" }, ...facts.flatMap(([label, value]) => [h("dt", {}, label), h("dd", {}, value)])),
    h(
      "p",
      { class: "field-hint" },
      cost.dataRead
        ? "Sent: the structure, your notes and the sampled value hints. Columns that look like secrets are left out."
        : "Sent: the structure and your notes; no row values. Columns that look like secrets are left out.",
    ),
  ];
}
