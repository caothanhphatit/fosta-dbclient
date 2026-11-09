import logoUrl from "../assets/logo.svg";
import { h, icon, type IconName } from "../core/dom";

const SHORTCUTS: [string, string][] = [
  ["Ctrl+N", "New connection"],
  ["Ctrl+T", "New query"],
  ["Ctrl+Shift+A", "Assistant panel"],
  ["Ctrl+B", "Show or hide the sidebar"],
  ["Ctrl+Shift+B", "Show or hide the workspaces"],
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
  openCommunity(): void;
}

/** The screen shown when no tab is open. `workspaceName` is updated by the caller. */
export function welcomeScreen(workspaceName: HTMLElement, actions: WelcomeActions): HTMLElement {
  const card = (iconName: IconName, title: string, run: () => void) => {
    const button = h("button", { class: "welcome-action", type: "button" }, icon(iconName), h("span", { class: "welcome-action-title" }, title));
    button.addEventListener("click", run);
    return button;
  };
  return h(
    "div",
    { class: "welcome" },
    h(
      "div",
      { class: "welcome-inner" },
      h("div", { class: "welcome-brand" }, h("img", { class: "welcome-icon", src: logoUrl, alt: "" }), h("h1", {}, "Fosta Studio")),
      h("p", { class: "welcome-sub" }, "Workspace ", workspaceName),
      h(
        "div",
        { class: "welcome-actions" },
        card("plug", "New connection", () => actions.newConnection()),
        card("query", "New query", () => actions.newQuery()),
        requires(
          card("task", "New command", () => actions.newTask()),
          "tasks",
        ),
      ),
      h("p", { class: "welcome-sub" }, h("button", { class: "link-button", type: "button", onclick: () => actions.openCommunity() }, "Community (Discord)")),
      h("ul", { class: "shortcuts" }, ...SHORTCUTS.map(([keys, action]) => h("li", {}, h("kbd", {}, keys), h("span", {}, action)))),
    ),
  );
}

/** Hidden by `hideUnsupported` when the edition lacks `capability` (commands in the web edition). */
function requires(element: HTMLElement, capability: string): HTMLElement {
  element.dataset.requires = capability;
  return element;
}
