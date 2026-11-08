import { h, icon, type IconName } from "../core/dom";
import { visibleEngines } from "../core/engines";

const SHORTCUTS: [string, string][] = [
  ["Ctrl+N", "New connection"],
  ["Ctrl+T", "New query"],
  ["Ctrl+Shift+A", "Assistant panel"],
  ["Ctrl+1 … 9", "Switch workspace"],
  ["Ctrl+Enter", "Run SQL (selection or all)"],
  ["Enter / F2", "Edit the selected cell"],
  ["Ctrl+S", "Save grid changes"],
  ["F5", "Reload table"],
];

export interface WelcomeActions {
  newConnection(): void;
  newQuery(): void;
  newTask(): void;
}

/** The screen shown when no tab is open. `workspaceName` is updated by the caller. */
export function welcomeScreen(workspaceName: HTMLElement, actions: WelcomeActions): HTMLElement {
  const card = (iconName: IconName, title: string, detail: string, run: () => void, detailClass = "") => {
    const button = h(
      "button",
      { class: "welcome-action", type: "button" },
      icon(iconName),
      h("span", { class: "welcome-action-title" }, title),
      h("span", { class: `welcome-action-detail ${detailClass}`.trim() }, detail),
    );
    button.addEventListener("click", run);
    return button;
  };
  return h(
    "div",
    { class: "welcome" },
    h(
      "div",
      { class: "welcome-inner" },
      h("div", { class: "welcome-brand" }, icon("database", "welcome-icon"), h("h1", {}, "Fosta Studio")),
      h("p", { class: "welcome-sub" }, "Workspace ", workspaceName),
      h(
        "div",
        { class: "welcome-actions" },
        card("plug", "New connection", "", () => actions.newConnection(), "welcome-engines"),
        card("query", "New query", "On the selected connection", () => actions.newQuery()),
        requires(
          card("task", "New startup task", "Port-forward or other background command", () => actions.newTask()),
          "tasks",
        ),
      ),
      h("ul", { class: "shortcuts" }, ...SHORTCUTS.map(([keys, action]) => h("li", {}, h("kbd", {}, keys), h("span", {}, action)))),
    ),
  );
}

/** Fills the engine list of the connection card from the registry; call once the descriptors are loaded. */
export function showWelcomeEngines(welcome: HTMLElement): void {
  const target = welcome.querySelector(".welcome-engines");
  if (target)
    target.textContent = visibleEngines()
      .map((engine) => engine.label)
      .join(", ");
}

/** Hidden by `hideUnsupported` when the edition lacks `capability` (startup tasks in the web edition). */
function requires(element: HTMLElement, capability: string): HTMLElement {
  element.dataset.requires = capability;
  return element;
}
