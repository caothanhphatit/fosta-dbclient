import { h, icon, type IconName } from "../core/dom";

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
  const card = (iconName: IconName, title: string, detail: string, run: () => void) => {
    const button = h(
      "button",
      { class: "welcome-action", type: "button" },
      icon(iconName),
      h("span", { class: "welcome-action-title" }, title),
      h("span", { class: "welcome-action-detail" }, detail),
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
      h("p", { class: "welcome-sub" }, "Workspace ", workspaceName, ". Connect on the left, then open a table or a query."),
      h(
        "div",
        { class: "welcome-actions" },
        card("plug", "New connection", "Save a MariaDB or MySQL server in this workspace", () => actions.newConnection()),
        card("query", "New query", "Open a SQL editor on the selected connection", () => actions.newQuery()),
        requires(
          card("task", "New startup task", "Keep a port-forward alive while this workspace is open", () => actions.newTask()),
          "tasks",
        ),
      ),
      h("ul", { class: "shortcuts" }, ...SHORTCUTS.map(([keys, action]) => h("li", {}, h("kbd", {}, keys), h("span", {}, action)))),
    ),
  );
}

/** Hidden by `hideUnsupported` when the edition lacks `capability` (startup tasks in the web edition). */
function requires(element: HTMLElement, capability: string): HTMLElement {
  element.dataset.requires = capability;
  return element;
}
