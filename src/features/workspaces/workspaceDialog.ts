import { api } from "../../core/api";
import { field, openModal, textInput } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import type { AppConfig, WorkspaceMeta } from "../../core/types";
import { newWorkspaceSafety, PRODUCTION_ICON, relaxations, safetyOf } from "../safety/safety";
import { relaxationWarning, safetyEditor } from "../safety/safetySection";
import { WORKSPACE_COLORS, WORKSPACE_ICONS, workspaceColor, workspaceIcon } from "./workspaceRail";

export function workspaceDialog(existing: WorkspaceMeta | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Production, Client X, Local" });
    let color = workspaceColor(existing?.color ?? WORKSPACE_COLORS[Math.floor(Math.random() * WORKSPACE_COLORS.length)]);
    let iconName = workspaceIcon(existing?.icon ?? "laptop");
    const preview = h("div", { class: `ws-preview color-${color}` }, icon(iconName));
    const refreshPreview = () => {
      preview.className = `ws-preview color-${color}`;
      preview.replaceChildren(icon(iconName));
    };
    // The rules as saved (or the default for a new workspace): what "less safe" is measured against.
    const savedSafety = () => (existing ? safetyOf(existing) : newWorkspaceSafety(iconName));
    const warningHost = h("div", { class: "relax-host" });
    const relaxName = textInput("", { class: "input mono", placeholder: existing?.name ?? "" });
    const relaxField = field("", relaxName, "Required to relax a Production workspace.");
    const isProduction = () => iconName === PRODUCTION_ICON || existing?.icon === PRODUCTION_ICON;
    const refreshWarning = () => {
      const warning = relaxationWarning(savedSafety(), safety.value(), isProduction());
      const needsName = warning !== null && isProduction();
      const label = relaxField.querySelector(".field-label");
      if (label) label.textContent = `Type ${existing?.name ?? (name.value.trim() || "the workspace name")} to confirm`;
      const shown = warningHost.childElementCount > 0;
      warningHost.replaceChildren(...(warning ? [warning] : []), ...(needsName ? [relaxField] : []));
      // The warning sits below the rules editor; bring it into view when it first appears.
      if (warning && !shown && warningHost.isConnected) requestAnimationFrame(() => warningHost.scrollIntoView({ block: "nearest", behavior: "smooth" }));
    };
    const safety = safetyEditor(savedSafety(), refreshWarning);
    const swatches = WORKSPACE_COLORS.map((option) => {
      const swatch = h("button", { class: `swatch color-${option}${option === color ? " active" : ""}`, type: "button", title: option, "aria-label": option });
      swatch.addEventListener("click", () => {
        color = option;
        for (const item of swatches) item.classList.toggle("active", item === swatch);
        refreshPreview();
      });
      return swatch;
    });
    const icons = WORKSPACE_ICONS.map((option) => {
      const button = h(
        "button",
        { class: `icon-choice${option.name === iconName ? " active" : ""}`, type: "button", title: option.label, "aria-label": option.label },
        icon(option.name),
        h("span", {}, option.label),
      );
      button.addEventListener("click", () => {
        iconName = option.name;
        for (const item of icons) item.classList.toggle("active", item === button);
        refreshPreview();
        // A workspace marked Production defaults to Read-only (unless the user picked rules here).
        if (iconName === PRODUCTION_ICON) safety.suggest("readOnly");
        refreshWarning();
      });
      return button;
    });

    const body = h(
      "div",
      { class: "form-grid" },
      h("div", { class: "ws-preview-row" }, preview, field("Workspace name", name)),
      field("Kind", h("div", { class: "icon-choices" }, ...icons)),
      field("Color", h("div", { class: "swatches" }, ...swatches)),
      h(
        "div",
        { class: "field" },
        h("span", { class: "field-label" }, "Safety"),
        h("span", { class: "field-hint" }, "What query tabs, grid edits, imports and filters may run here"),
        safety.element,
      ),
      warningHost,
    );
    openModal(
      existing ? "Edit workspace" : "New workspace",
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: existing ? "Save" : "Create",
          primary: true,
          run: async () => {
            const trimmed = name.value.trim();
            if (!trimmed) throw new Error("Workspace name is required");
            const rules = safety.value();
            const relaxing = relaxations(savedSafety(), rules).length > 0;
            const expected = existing?.name ?? trimmed;
            if (relaxing && isProduction() && relaxName.value.trim() !== expected.trim()) {
              relaxName.scrollIntoView({ block: "center" });
              relaxName.focus();
              throw new Error(`Type ${expected} to relax the rules of a Production workspace`);
            }
            saved = await api.saveWorkspace({ id, name: trimmed, color, icon: iconName, safety: rules }, relaxing ? relaxName.value.trim() : null);
          },
        },
      ],
      { width: 620, onClose: () => resolve(saved) },
    );
  });
}
