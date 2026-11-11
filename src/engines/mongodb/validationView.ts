/**
 * The Validation section of a collection workspace: the collection's rules (`mongo.validation`): level and action, and
 * the validator in a mongosh-syntax editor. "Generate from schema" builds a `$jsonSchema` from the tab's schema analysis
 * (`mongo.schema`, analysed here when the Schema section has not run): types per path (mixed: a list), `required` for
 * fields present in at least the chosen share, nested objects and array items, optionally enums for small string sets.
 * "Check existing documents" counts the documents the draft refuses (`mongo.validationCheck`) with a few samples; a
 * sample opens Documents filtered by its `_id`. Apply shows the diff of the old and new validator with warnings (failing
 * documents under strict rules, rules removed) and runs `mongo.setValidation` through the gateway (DDL). Discard goes
 * back to the applied rules; Remove rules empties the validator.
 *
 * The draft is kept with the tab while it differs; the assistant gets the rules in a few lines.
 */
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import { toast } from "../../core/dialogs";
import { copyText, formatNumber, h, icon } from "../../core/dom";
import { errorText } from "../../core/errors";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { queryBarCompletionSource } from "../../lang/mongo/queryCompletion";
import { SchemaCache } from "../../lang/schemaCache";
import { lineDiff } from "./assistantModel";
import { shellText } from "./bson";
import type { DocumentsRequest } from "./savedQueries";
import { MongoApi } from "./mongoApi";
import { StageEditor } from "./queryEditor";
import { readResult, STORE_SCHEMA, TYPE_LABELS, type SchemaResult } from "./schemaModel";
import {
  ACTIONS,
  applyShell,
  applyWarnings,
  draftChanges,
  failingFilter,
  generateSchema,
  idFilter,
  isDirty,
  LEVELS,
  MAX_ENUM,
  parseValidator,
  readRules,
  readValidationState,
  rulesBrief,
  savedValidationState,
  schemaText,
  setArgs,
  validatorText,
  type Draft,
  type GenerateOptions,
  type SetValidationArgs,
  type ValidationAction,
  type ValidationCheckResult,
  type ValidationLevel,
  type ValidationRules,
} from "./validationModel";
import "./mongodb.css";
import "./validation.css";

export function validationSectionFactory(ctx: SectionContext): SectionContent {
  return new ValidationSection(ctx);
}

/** Documents sampled when Generate has to analyse the schema itself. */
const GENERATE_SAMPLE = 1000;
const CHECK_SAMPLES = 5;

const JSON_SCHEMA_KEYS: [string, string][] = [
  ["bsonType", "BSON type, or a list of types"],
  ["required", "Fields that must be present"],
  ["properties", "Rules per field"],
  ["additionalProperties", "false: no other fields allowed"],
  ["items", "Rules for array elements"],
  ["enum", "Allowed values"],
  ["minimum", "Smallest number"],
  ["maximum", "Largest number"],
  ["exclusiveMinimum", "minimum excluded"],
  ["exclusiveMaximum", "maximum excluded"],
  ["minLength", "Shortest string"],
  ["maxLength", "Longest string"],
  ["pattern", "Regular expression strings must match"],
  ["minItems", "Fewest array elements"],
  ["maxItems", "Most array elements"],
  ["uniqueItems", "Array elements are distinct"],
  ["description", "Shown in the error of a refused write"],
  ["title", "A name for the rule"],
  ["anyOf", "Any of these schemas"],
  ["oneOf", "Exactly one of these schemas"],
  ["allOf", "All of these schemas"],
  ["not", "Must not match this schema"],
];
const BSON_TYPES = [...new Set(Object.keys(TYPE_LABELS).filter((type) => type !== "javascriptWithScope" && type !== "dbPointer"))];

/** Completion: `$jsonSchema` keywords and BSON type names inside a `$jsonSchema`, query operators and fields elsewhere. */
function validatorCompletion(query: CompletionSource): CompletionSource {
  return (context: CompletionContext): CompletionResult | null | Promise<CompletionResult | null> => {
    const before = context.state.doc.sliceString(0, context.pos);
    if (before.includes("$jsonSchema")) {
      const typeValue = /bsonType["']?\s*:\s*(?:\[[^\]]*?)?(["'])(\w*)$/.exec(before);
      if (typeValue) {
        const close = context.state.doc.sliceString(context.pos, context.pos + 1) === typeValue[1] ? "" : typeValue[1];
        const options: Completion[] = BSON_TYPES.map((type) => ({ label: type, apply: type + close, type: "type", detail: TYPE_LABELS[type] }));
        return { from: context.pos - typeValue[2].length, options, validFor: /^\w*$/ };
      }
      const keyword = /[{,]\s*([\w$]*)$/.exec(before);
      if (keyword && (context.explicit || keyword[1].length > 0)) {
        const options: Completion[] = JSON_SCHEMA_KEYS.map(([label, info]) => ({ label, apply: `${label}: `, info, type: "keyword", detail: "$jsonSchema" }));
        return { from: context.pos - keyword[1].length, options, validFor: /^[\w$]*$/ };
      }
    }
    return query(context);
  };
}

class ValidationSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly database: string;
  private readonly collection: string;
  private readonly completionSchema = new SchemaCache();
  private readonly meta = h("span", { class: "vd-meta" });
  private readonly info = h("div", { class: "vd-info", role: "status", hidden: true });
  private readonly levelSelect: HTMLSelectElement;
  private readonly actionSelect: HTMLSelectElement;
  private readonly refreshButton: HTMLButtonElement;
  private readonly editorHost = h("div", { class: "vd-editor" });
  private readonly editor: StageEditor;
  private readonly parseError = h("div", { class: "vd-parse-error", role: "alert", hidden: true });
  private readonly dirtyBadge = h("span", { class: "vd-dirty", hidden: true }, "Not applied");
  private readonly generateHost = h("div", { class: "vd-generate", hidden: true });
  private readonly checkHost = h("section", { class: "vd-check", "aria-label": "Existing documents", hidden: true });
  private readonly reviewHost = h("section", { class: "vd-review", "aria-label": "Review the changes", hidden: true });
  private readonly checkButton: HTMLButtonElement;
  private readonly applyButton: HTMLButtonElement;
  private readonly discardButton: HTMLButtonElement;
  private rules: ValidationRules | null = null;
  private generate: GenerateOptions;
  private check: ValidationCheckResult | null = null;
  /** The validator text the last check ran on (its result holds while the draft is the same). */
  private checkedText: string | null = null;
  private busy = false;
  private lastError: string | null = null;
  private generation = 0;
  private disposed = false;
  /** The draft restored from the saved tab, applied over the rules once they are read. */
  private restored: Draft | null;

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    void this.completionSchema.load(() => ctx.view.connection.completionSchema(this.database || null), this.database).catch(() => undefined);
    const saved = readValidationState(ctx.savedState);
    this.generate = saved.generate;
    this.restored = saved.draft;

    const select = <T extends string>(label: string, items: readonly { value: T; label: string; title: string }[], value: T) => {
      const element = h("select", { class: "input vd-select", "aria-label": label, title: items.find((item) => item.value === value)?.title ?? "" });
      for (const item of items) {
        const option = h("option", { value: item.value, title: item.title }, item.label);
        option.selected = item.value === value;
        element.append(option);
      }
      element.addEventListener("change", () => {
        element.title = items.find((item) => item.value === element.value)?.title ?? "";
        this.draftChanged();
      });
      return element;
    };
    this.levelSelect = select("Validation level", LEVELS, saved.draft?.level ?? "strict");
    this.actionSelect = select("Validation action", ACTIONS, saved.draft?.action ?? "error");
    this.refreshButton = h("button", { class: "btn", type: "button", title: "Read the rules again" }, icon("refresh"), h("span", {}, "Refresh"));
    this.refreshButton.addEventListener("click", () => void this.load(true));
    const generateButton = h("button", { class: "btn", type: "button", "aria-expanded": "false" }, icon("sparkle"), h("span", {}, "Generate from schema"));
    generateButton.addEventListener("click", () => {
      this.generateHost.hidden = !this.generateHost.hidden;
      generateButton.setAttribute("aria-expanded", String(!this.generateHost.hidden));
      if (!this.generateHost.hidden) this.renderGenerate();
    });
    const bar = h(
      "div",
      { class: "vd-bar" },
      h("label", { class: "vd-field" }, h("span", { class: "vd-label" }, "Level"), this.levelSelect),
      h("label", { class: "vd-field" }, h("span", { class: "vd-label" }, "Action"), this.actionSelect),
      this.dirtyBadge,
      h("div", { class: "toolbar-end" }, this.meta, generateButton, this.refreshButton),
    );

    const query = queryBarCompletionSource({ field: "filter", collection: this.collection, schema: () => this.completionSchema.get() });
    this.editor = new StageEditor(this.editorHost, {
      doc: saved.draft?.validator ?? "",
      label: "Validator (mongosh syntax)",
      completion: validatorCompletion(query),
      run: () => void this.runCheck(),
      changed: () => this.draftChanged(),
    });

    this.checkButton = h(
      "button",
      { class: "btn", type: "button", title: "Count the documents these rules refuse (Ctrl+Enter)" },
      icon("search"),
      h("span", {}, "Check existing documents"),
    );
    this.checkButton.addEventListener("click", () => void this.runCheck());
    const copy = h("button", { class: "btn", type: "button", title: "Copy the validator" }, icon("copy"), h("span", {}, "Copy"));
    copy.addEventListener("click", () => void copyText(this.editor.text).then(() => toast("Copied the validator", "success")));
    const remove = h(
      "button",
      { class: "btn", type: "button", title: "Empty the validator (applying removes the rules)" },
      icon("trash"),
      h("span", {}, "Remove rules"),
    );
    remove.addEventListener("click", () => {
      this.editor.text = "";
      this.draftChanged();
    });
    this.discardButton = h("button", { class: "btn", type: "button", title: "Go back to the applied rules", disabled: true }, h("span", {}, "Discard"));
    this.discardButton.addEventListener("click", () => this.discard());
    this.applyButton = h("button", { class: "btn primary", type: "button", disabled: true }, icon("check"), h("span", {}, "Review and apply"));
    this.applyButton.addEventListener("click", () => void this.review());
    const actions = h("div", { class: "vd-actions" }, this.checkButton, copy, remove, h("span", { class: "spacer" }), this.discardButton, this.applyButton);

    const body = h(
      "div",
      { class: "vd-body" },
      this.generateHost,
      h(
        "div",
        { class: "vd-editor-head" },
        h("strong", {}, "Validator"),
        h("span", { class: "vd-dim" }, "mongosh syntax: a $jsonSchema and/or query operators; empty: no rules"),
      ),
      this.editorHost,
      this.parseError,
      actions,
      this.reviewHost,
      this.checkHost,
    );
    this.element = h("div", { class: "view vd-section" }, bar, this.info, body);
    this.updateButtons();
    void this.load(false);
  }

  dispose(): void {
    this.disposed = true;
    this.editor.destroy();
  }

  saveState(): unknown {
    const draft = this.draft();
    return savedValidationState(this.rules && !isDirty(this.applied(), draft) ? null : draft, this.generate);
  }

  assistantContext(): string | null {
    const lines = [
      rulesBrief(
        { database: this.database, collection: this.collection },
        this.rules,
        this.draft(),
        this.check && this.checkedText === this.editor.text ? this.check : null,
      ),
    ];
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  // --- Rules --------------------------------------------------------------------------------------------------------

  private draft(): Draft {
    return { validator: this.editor.text, level: this.levelSelect.value as ValidationLevel, action: this.actionSelect.value as ValidationAction };
  }

  private applied(): Draft {
    const rules = this.rules;
    return { validator: validatorText(rules?.validator ?? null), level: rules?.validationLevel ?? "strict", action: rules?.validationAction ?? "error" };
  }

  private setDraft(draft: Draft): void {
    this.editor.text = draft.validator;
    this.levelSelect.value = draft.level;
    this.actionSelect.value = draft.action;
    this.draftChanged();
  }

  private async load(ask: boolean): Promise<void> {
    const generation = ++this.generation;
    this.refreshButton.disabled = true;
    try {
      const raw = await this.api.validation(ask);
      if (this.disposed || generation !== this.generation) return;
      const rules = readRules(raw);
      if (!rules) {
        if (!this.rules) this.showInfo("Press Refresh to read the validation rules.", "info");
        return;
      }
      const wasDirty = this.rules ? isDirty(this.applied(), this.draft()) : false;
      this.rules = rules;
      this.lastError = null;
      this.showInfo(rules.type === "view" ? "This is a view: views have no validation rules." : null, "info");
      const restored = this.restored;
      this.restored = null;
      if (restored && isDirty(this.applied(), restored)) {
        this.setDraft(restored);
        this.showInfo("Restored a draft that is not applied yet.", "info");
      } else if (!wasDirty) this.setDraft(this.applied());
      else this.draftChanged();
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.lastError = errorText(error);
      this.showInfo(ask || this.rules ? this.lastError : "Press Refresh to read the validation rules.", ask || this.rules ? "error" : "info");
    } finally {
      if (generation === this.generation && !this.disposed) {
        this.refreshButton.disabled = false;
        this.drawMeta();
        this.updateButtons();
      }
    }
  }

  private drawMeta(): void {
    const rules = this.rules;
    this.meta.textContent = rules
      ? `Applied: ${rules.validator ? "validator" : "no validator"} · ${rules.validationLevel} · ${rules.validationAction} · ${formatNumber(rules.elapsedMs)} ms`
      : "";
  }

  private showInfo(message: string | null, kind: "info" | "error" | "warning"): void {
    this.info.hidden = !message;
    this.info.className = `vd-info${kind === "info" ? "" : ` vd-info-${kind}`}`;
    this.info.replaceChildren(...(message ? [icon(kind === "error" ? "x" : "health", "vd-info-icon"), h("span", {}, message)] : []));
  }

  private draftChanged(): void {
    this.parseError.hidden = true;
    this.editor.showError(null);
    this.reviewHost.hidden = true;
    this.reviewHost.replaceChildren();
    if (this.check && this.checkedText !== this.editor.text) this.checkHost.classList.add("vd-stale");
    else this.checkHost.classList.remove("vd-stale");
    this.updateButtons();
    this.ctx.stateChanged();
  }

  private updateButtons(): void {
    const dirty = !!this.rules && isDirty(this.applied(), this.draft());
    this.dirtyBadge.hidden = !dirty;
    this.discardButton.disabled = !dirty || this.busy;
    this.applyButton.disabled = !dirty || this.busy || this.rules?.type === "view";
    this.checkButton.disabled = this.busy;
  }

  private discard(): void {
    this.setDraft(this.applied());
    this.showInfo(null, "info");
  }

  /** The draft's validator for the backend, or null after showing where it does not parse. */
  private parsedDraft(): { json: string; empty: boolean } | null {
    const parsed = parseValidator(this.editor.text);
    if (parsed.ok) return parsed;
    this.parseError.hidden = false;
    this.parseError.textContent = parsed.error;
    this.editor.showError(parsed.position);
    this.editor.focus(parsed.position);
    return null;
  }

  // --- Generate -----------------------------------------------------------------------------------------------------

  private renderGenerate(): void {
    const percent = h("input", {
      class: "input vd-number",
      type: "number",
      min: 0,
      max: 100,
      step: 5,
      value: String(this.generate.requiredPercent),
      "aria-label": "Required share",
    });
    percent.addEventListener("change", () => {
      this.generate.requiredPercent = Math.max(0, Math.min(100, Math.round(Number(percent.value) || 0)));
      percent.value = String(this.generate.requiredPercent);
      this.ctx.stateChanged();
    });
    const enumOn = h("input", { type: "checkbox", checked: this.generate.enumMax > 0 });
    const enumMax = h("input", {
      class: "input vd-number",
      type: "number",
      min: 1,
      max: MAX_ENUM,
      value: String(this.generate.enumMax || 5),
      disabled: this.generate.enumMax === 0,
      "aria-label": "Most enum values",
    });
    const enumChanged = () => {
      enumMax.disabled = !enumOn.checked;
      this.generate.enumMax = enumOn.checked ? Math.max(1, Math.min(MAX_ENUM, Math.round(Number(enumMax.value) || 5))) : 0;
      if (enumOn.checked) enumMax.value = String(this.generate.enumMax);
      this.ctx.stateChanged();
    };
    enumOn.addEventListener("change", enumChanged);
    enumMax.addEventListener("change", enumChanged);
    const schema = readResult(this.ctx.store.get<SchemaResult>(STORE_SCHEMA));
    const source = schema
      ? `From the Schema section's ${schema.sample.mode} sample of ${formatNumber(schema.sample.count)} documents${schema.truncated ? " (paths truncated)" : ""}.`
      : `No schema analysed in this tab yet: a random sample of ${formatNumber(GENERATE_SAMPLE)} documents is analysed first.`;
    const run = h("button", { class: "btn primary", type: "button" }, icon("sparkle"), h("span", {}, "Generate"));
    run.addEventListener("click", () => void this.runGenerate(run));
    this.generateHost.replaceChildren(
      h(
        "div",
        { class: "vd-generate-row" },
        h(
          "label",
          { class: "vd-field", title: "A field is required when present in at least this share of the documents (of its parent document for nested fields)" },
          h("span", {}, "Required when present in ≥"),
          percent,
          h("span", {}, "%"),
        ),
        h(
          "label",
          { class: "vd-check-label", title: "String fields whose sampled values all fit get an enum of them" },
          enumOn,
          h("span", {}, "Enum for string fields with ≤"),
          enumMax,
          h("span", {}, "values"),
        ),
        h("span", { class: "spacer" }),
        run,
      ),
      h("div", { class: "vd-dim" }, source, " The generated validator replaces the editor's text; nothing is applied until you apply it."),
    );
  }

  private async runGenerate(button: HTMLButtonElement): Promise<void> {
    button.disabled = true;
    try {
      let schema = readResult(this.ctx.store.get<SchemaResult>(STORE_SCHEMA));
      if (!schema) {
        const analysed = await this.api.schema(
          { database: this.database, collection: this.collection, sampleMode: "random", sampleSize: GENERATE_SAMPLE, references: false },
          true,
        );
        if (!analysed || this.disposed) return;
        schema = readResult(analysed);
      }
      if (!schema || schema.sample.count === 0) {
        this.showInfo("The collection has no documents to generate rules from.", "info");
        return;
      }
      this.editor.text = validatorText(generateSchema(schema, this.generate));
      this.draftChanged();
      this.showInfo(`Generated from ${formatNumber(schema.sample.count)} sampled documents. Check existing documents before applying.`, "info");
    } catch (error) {
      if (this.disposed) return;
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
    } finally {
      button.disabled = false;
    }
  }

  // --- Check --------------------------------------------------------------------------------------------------------

  private async runCheck(): Promise<ValidationCheckResult | null> {
    const parsed = this.parsedDraft();
    if (!parsed || this.busy) return null;
    const text = this.editor.text;
    this.busy = true;
    this.updateButtons();
    try {
      const result = await this.api.validationCheck({ database: this.database, collection: this.collection, validator: parsed.json, limit: CHECK_SAMPLES });
      if (!result || this.disposed) return null;
      this.check = result;
      this.checkedText = text;
      this.lastError = null;
      this.renderCheck(parsed.json, parsed.empty);
      return result;
    } catch (error) {
      if (this.disposed) return null;
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
      return null;
    } finally {
      this.busy = false;
      if (!this.disposed) this.updateButtons();
    }
  }

  private renderCheck(validatorJson: string, empty: boolean): void {
    const result = this.check;
    if (!result) return;
    this.checkHost.hidden = false;
    this.checkHost.classList.remove("vd-stale");
    const total = result.total === null ? "" : ` of ${formatNumber(result.total)}`;
    const failing = `${formatNumber(result.failing)}${result.capped ? "+" : ""}`;
    const head = h(
      "div",
      { class: "vd-check-head" },
      h(
        "strong",
        { class: result.failing > 0 ? "vd-bad" : "vd-good" },
        empty ? "No rules: every document passes" : result.failing === 0 ? `All${total} documents pass` : `${failing}${total} documents would fail`,
      ),
      h("span", { class: "vd-dim" }, `${formatNumber(result.elapsedMs)} ms`),
      h("span", { class: "spacer" }),
    );
    if (result.failing > 0) {
      const all = h(
        "button",
        { class: "btn small", type: "button", title: "Open Documents filtered to the failing documents" },
        icon("filter"),
        h("span", {}, "Show in Documents"),
      );
      all.addEventListener("click", () => void this.ctx.deliver("documents", { filter: failingFilter(validatorJson) } satisfies DocumentsRequest));
      head.append(all);
    }
    const samples = h(
      "ul",
      { class: "vd-samples", "aria-label": "Failing documents" },
      ...result.samples.map((document) => {
        const id = document._id;
        const preview = shellText(Object.fromEntries(Object.entries(document).filter(([key]) => key !== "_id")));
        const item = h(
          "button",
          { class: "vd-sample", type: "button", title: "Open in Documents, filtered by this _id" },
          h("span", { class: "vd-sample-id mono" }, shellText(id ?? null)),
          h("span", { class: "vd-sample-doc mono" }, preview.length > 160 ? `${preview.slice(0, 159)}…` : preview),
        );
        item.addEventListener("click", () => void this.ctx.deliver("documents", { filter: idFilter(id ?? null) } satisfies DocumentsRequest));
        return h("li", {}, item);
      }),
    );
    this.checkHost.replaceChildren(head);
    if (result.note) this.checkHost.append(h("div", { class: "vd-dim" }, result.note));
    if (result.samples.length) this.checkHost.append(samples);
  }

  // --- Apply --------------------------------------------------------------------------------------------------------

  private async review(): Promise<void> {
    if (!this.rules || this.busy) return;
    const draft = this.draft();
    const applied = this.applied();
    const args = setArgs({ database: this.database, collection: this.collection }, applied, draft);
    if ("error" in args) {
      this.parsedDraft();
      return;
    }
    const changes = draftChanges(applied, draft);
    let check = this.checkedText === draft.validator ? this.check : null;
    const parsed = parseValidator(draft.validator);
    if (!check && parsed.ok && !parsed.empty && draft.level !== "off") check = await this.runCheck();
    if (this.disposed) return;
    const warnings = applyWarnings(draft, check ? { count: check.failing, capped: check.capped } : null);
    const blocks: HTMLElement[] = [
      h("div", { class: "vd-review-head" }, h("strong", {}, "Review the changes"), h("span", { class: "vd-dim" }, `on ${this.database}.${this.collection}`)),
    ];
    const settings: string[] = [];
    if (changes.level) settings.push(`Level: ${applied.level} → ${draft.level}`);
    if (changes.action) settings.push(`Action: ${applied.action} → ${draft.action}`);
    if (settings.length) blocks.push(h("div", { class: "vd-review-settings" }, ...settings.map((line) => h("div", {}, line))));
    if (changes.validator) {
      const before = applied.validator.trim();
      const after = parsed.ok && !parsed.empty ? schemaText(JSON.parse(parsed.json) as unknown) : "";
      const marks = { same: "  ", removed: "- ", added: "+ " } as const;
      blocks.push(
        h(
          "pre",
          { class: "mq-diff vd-diff", "aria-label": "Validator changes: removed lines start with -, added lines with +" },
          ...lineDiff(before || "(no validator)", after || "(no validator)").map((line) =>
            h("div", { class: `mq-diff-line mq-diff-${line.kind}` }, `${marks[line.kind]}${line.text}`),
          ),
        ),
      );
    }
    for (const warning of warnings) blocks.push(h("div", { class: "vd-warning", role: "alert" }, icon("health"), h("span", {}, warning)));
    if (draft.level === "strict" && draft.action === "error" && check && check.failing > 0) {
      const moderate = h("button", { class: "btn small", type: "button" }, "Use Moderate instead");
      moderate.addEventListener("click", () => {
        this.levelSelect.value = "moderate";
        this.draftChanged();
        void this.review();
      });
      blocks.push(h("div", { class: "vd-review-alt" }, moderate));
    }
    blocks.push(h("pre", { class: "vd-shell mono", "aria-label": "mongosh command" }, applyShell(this.collection, args)));
    const cancel = h("button", { class: "btn", type: "button" }, "Cancel");
    cancel.addEventListener("click", () => {
      this.reviewHost.hidden = true;
      this.reviewHost.replaceChildren();
    });
    const apply = h("button", { class: "btn primary", type: "button" }, icon("check"), h("span", {}, "Apply"));
    apply.addEventListener("click", () => void this.apply(args));
    blocks.push(h("div", { class: "vd-actions" }, h("span", { class: "spacer" }), cancel, apply));
    this.reviewHost.replaceChildren(...blocks);
    this.reviewHost.hidden = false;
    apply.focus();
  }

  private async apply(args: SetValidationArgs): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.updateButtons();
    try {
      const result = await this.api.setValidation(args);
      if (!result || this.disposed) return;
      this.reviewHost.hidden = true;
      this.reviewHost.replaceChildren();
      toast(args.validator === "{}" ? "Removed the validation rules" : "Applied the validation rules", "success");
      this.rules = null;
      this.busy = false;
      await this.load(false);
    } catch (error) {
      if (this.disposed) return;
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
    } finally {
      this.busy = false;
      if (!this.disposed) this.updateButtons();
    }
  }
}
