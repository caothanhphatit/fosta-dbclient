/**
 * The connection dialog's "Command" select: none, one of the workspace's commands (started before connecting when it
 * does not run, see `src-tauri/src/connection_command.rs`), or "New command…" (the command dialog, then selected).
 * Hidden where commands are off (web edition without FOSTA_STUDIO_TASKS); a saved choice is then kept as it is.
 */
import { capabilities } from "../../core/capabilities";
import { field } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { TaskDef } from "../../core/types";

const NONE = "";
const NEW = "\u0000new";

export interface CommandChoices {
  /** The commands of the connection's workspace. */
  commands: TaskDef[];
  /** Opens the command dialog for this connection; the created command, or null when cancelled. */
  create(): Promise<TaskDef | null>;
}

export interface CommandPicker {
  element: HTMLElement;
  read(): string | null;
}

export function renderCommandPicker(saved: string | null | undefined, choices: CommandChoices, created: (task: TaskDef) => void): CommandPicker {
  const initial = saved ?? null;
  if (!capabilities().tasks) return { element: h("div", { hidden: true }), read: () => initial };
  const option = (task: TaskDef) => h("option", { value: task.id }, task.name);
  const known = choices.commands.some((task) => task.id === initial);
  const select = h(
    "select",
    { class: "input" },
    h("option", { value: NONE }, "None"),
    ...choices.commands.map(option),
    h("option", { value: NEW }, "New command…"),
  );
  select.value = known && initial ? initial : NONE;
  let previous = select.value;
  select.addEventListener("change", () => {
    if (select.value !== NEW) {
      previous = select.value;
      return;
    }
    select.value = previous;
    void choices.create().then((task) => {
      if (!task) return;
      select.insertBefore(option(task), select.lastElementChild);
      select.value = previous = task.id;
      created(task);
    });
  });
  return {
    element: field("Command", select, "Runs before connecting, e.g. kubectl port-forward"),
    read: () => (select.value && select.value !== NEW ? select.value : null),
  };
}
