import { api } from "../../core/api";
import { field, openModal, textInput } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import type { AppConfig, WorkspaceMeta } from "../../core/types";
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
      const button = h("button", { class: `icon-choice${option.name === iconName ? " active" : ""}`, type: "button", title: option.label, "aria-label": option.label }, icon(option.name), h("span", {}, option.label));
      button.addEventListener("click", () => {
        iconName = option.name;
        for (const item of icons) item.classList.toggle("active", item === button);
        refreshPreview();
      });
      return button;
    });

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "A workspace keeps its own connections and startup tasks. Switch between them from the rail on the left."),
      h("div", { class: "ws-preview-row" }, preview, field("Workspace name", name)),
      field("Kind", h("div", { class: "icon-choices" }, ...icons)),
      field("Color", h("div", { class: "swatches" }, ...swatches)),
    );
    openModal(existing ? "Edit workspace" : "New workspace", body, [
      { label: "Cancel", run: () => undefined },
      {
        label: existing ? "Save" : "Create",
        primary: true,
        run: async () => {
          const trimmed = name.value.trim();
          if (!trimmed) throw new Error("Workspace name is required");
          saved = await api.saveWorkspace({ id, name: trimmed, color, icon: iconName });
        },
      },
    ], { width: 480, onClose: () => resolve(saved) });
  });
}
