/**
 * The "Safety" section of the workspace dialog: a preset (Open, Guarded, Read-only, Custom),
 * the Custom matrix (category → allow / confirm / typed confirm / block) and options, plus
 * a loud warning listing what becomes possible when the rules get less strict.
 */
import { checkbox, field, textInput } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import type { CategoryRules, SafetyAction, SafetyPreset, SafetySettings } from "../../core/types";
import {
  ACTION_LABELS,
  ACTIONS,
  CATEGORY_HINTS,
  CATEGORY_LABELS,
  PRESET_DESCRIPTIONS,
  PRESET_LABELS,
  PRESETS,
  presetSettings,
  relaxations,
  riskExamples,
  RULE_CATEGORIES,
} from "./safety";

export interface SafetyEditor {
  readonly element: HTMLElement;
  value(): SafetySettings;
  /** Switches the preset unless the user already changed it here (e.g. Production → Read-only). */
  suggest(preset: SafetyPreset): void;
}

export function safetyEditor(initial: SafetySettings, onChange: () => void): SafetyEditor {
  let current: SafetySettings = { ...initial, rules: { ...initial.rules } };
  let touched = false;
  const presetButtons = PRESETS.map((preset) => {
    const button = h(
      "button",
      { class: "preset-choice", type: "button", "aria-pressed": "false" },
      h("strong", {}, PRESET_LABELS[preset]),
      h("span", {}, PRESET_DESCRIPTIONS[preset]),
    );
    button.addEventListener("click", () => {
      touched = true;
      select(preset);
    });
    return button;
  });
  const selects = new Map<keyof CategoryRules, HTMLSelectElement>();
  const matrix = h(
    "table",
    { class: "safety-matrix" },
    h("thead", {}, h("tr", {}, h("th", {}, "Statements"), h("th", {}, "Rule"))),
    h(
      "tbody",
      {},
      ...RULE_CATEGORIES.map((category) => {
        const select = h(
          "select",
          { class: "input", "aria-label": `${CATEGORY_LABELS[category]} statements` },
          ...ACTIONS.map((action) => h("option", { value: action }, ACTION_LABELS[action])),
        );
        select.addEventListener("change", () => {
          touched = true;
          current = { ...current, rules: { ...current.rules, [category]: select.value as SafetyAction } };
          onChange();
        });
        selects.set(category, select);
        return h(
          "tr",
          {},
          h("td", {}, h("div", { class: "matrix-label" }, CATEGORY_LABELS[category]), h("div", { class: "field-hint" }, CATEGORY_HINTS[category])),
          h("td", {}, select),
        );
      }),
    ),
  );
  const multi = checkbox("Allow several statements per run in query tabs", current.allowMultiStatement);
  const timeout = textInput(String(current.statementTimeoutSecs), { type: "number", min: "0", class: "input narrow" });
  const maxRows = textInput(String(current.maxRows), { type: "number", min: "1", class: "input narrow" });
  multi.input.addEventListener("change", () => {
    touched = true;
    current = { ...current, allowMultiStatement: multi.input.checked };
    onChange();
  });
  timeout.addEventListener("input", () => {
    touched = true;
    current = { ...current, statementTimeoutSecs: Math.max(0, Math.floor(Number(timeout.value) || 0)) };
    onChange();
  });
  maxRows.addEventListener("input", () => {
    touched = true;
    current = { ...current, maxRows: Math.max(1, Math.floor(Number(maxRows.value) || 1)) };
    onChange();
  });
  const custom = h(
    "div",
    { class: "safety-custom" },
    matrix,
    multi.element,
    h(
      "div",
      { class: "form-row" },
      field("Statement timeout (s, 0 = none)", timeout, "MariaDB/MySQL and MongoDB browsing; not yet on SQL Server."),
      field("Max rows per result", maxRows),
    ),
  );

  function select(preset: SafetyPreset): void {
    current = presetSettings(preset, current);
    current.preset = preset;
    render();
    onChange();
  }

  function render(): void {
    presetButtons.forEach((button, index) => {
      const active = PRESETS[index] === current.preset;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    });
    for (const [category, element] of selects) element.value = current.rules[category];
    custom.hidden = current.preset !== "custom";
  }

  render();
  return {
    element: h("div", { class: "safety-editor" }, h("div", { class: "preset-choices" }, ...presetButtons), custom),
    value: () => ({ ...current, rules: { ...current.rules } }),
    suggest: (preset) => {
      if (!touched && current.preset !== preset) select(preset);
    },
  };
}

/** The warning shown while the edited rules are less strict than the saved ones; null when they are not. */
export function relaxationWarning(previous: SafetySettings, next: SafetySettings, production: boolean): HTMLElement | null {
  const changes = relaxations(previous, next);
  if (changes.length === 0) return null;
  const examples = riskExamples(changes);
  return h(
    "div",
    { class: "relax-warning", role: "alert" },
    h(
      "div",
      { class: "relax-title" },
      icon("shield"),
      production ? "You are relaxing the rules of a Production workspace" : "These rules are less safe than before",
    ),
    h("p", {}, "Becomes possible without the current safeguards:"),
    h("ul", {}, ...changes.map((change) => h("li", {}, change.text))),
    examples.length > 0 ? h("p", {}, "What could go wrong:") : null,
    examples.length > 0 ? h("ul", { class: "relax-examples" }, ...examples.map((example) => h("li", {}, example))) : null,
    h("p", { class: "field-hint" }, "The change is recorded in the local audit log, and the workspace shows a warning badge while it is relaxed."),
  );
}
