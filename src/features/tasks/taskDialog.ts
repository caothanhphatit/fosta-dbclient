/**
 * The command dialog ("New command" / "Edit command"): a workspace command (`TaskDef`, "task" in the config file) that
 * starts with the workspace or when a connection that uses it connects. Pure helpers live in `commandForm.ts`.
 */
import { api } from "../../core/api";
import { checkbox, field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { AppConfig, TaskDef } from "../../core/types";
import { commandValues, portForwardTemplate, taskFromValues, type CommandValues, type TemplateTarget } from "./commandForm";

export interface CommandDialogOptions {
  /** Opened from a connection: the template follows its engine and port, and the command starts when the connection needs it. */
  forConnection?: TemplateTarget;
  /** The workspace the command is saved in (default: the active one). */
  workspaceId?: string | null;
}

export interface SavedCommand {
  config: AppConfig;
  task: TaskDef;
}

const START_OPTIONS = [
  { value: "workspace", label: "Start with the workspace" },
  { value: "connection", label: "Start when a connection needs it" },
] as const;

export function taskDialog(existing: TaskDef | null, workspaceName: string, options: CommandDialogOptions = {}): Promise<SavedCommand | null> {
  return new Promise((resolve) => {
    let saved: SavedCommand | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const template = () => portForwardTemplate(options.forConnection ?? { engineId: "mariadb", engineLabel: "MariaDB", defaultPort: 3306 });
    const initial: CommandValues = existing ? commandValues(existing) : options.forConnection ? template() : commandValues(null);
    const name = textInput(initial.name);
    const program = textInput(initial.program, { class: "input mono" });
    const args = textInput(initial.args, { class: "input mono", placeholder: "port-forward -n <namespace> svc/<service> <local>:<remote>" });
    const readyPattern = textInput(initial.readyPattern, { placeholder: "Empty: when the connection's port accepts connections" });
    const timeout = textInput(initial.timeout, { type: "number", min: 1, max: 600 });
    const startWith = h("select", { class: "input" }, ...START_OPTIONS.map((option) => h("option", { value: option.value }, option.label)));
    startWith.value = (existing?.autoStart ?? !options.forConnection) ? "workspace" : "connection";
    const autoRestart = checkbox("Restart automatically when the process exits", existing?.autoRestart ?? true);
    const useTemplate = h("button", { class: "link-button", type: "button" }, "Fill kubectl port-forward template");
    useTemplate.addEventListener("click", () => {
      const values = template();
      name.value ||= values.name;
      program.value = values.program;
      args.value = values.args;
      readyPattern.value = values.readyPattern;
      timeout.value = values.timeout;
    });

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "A command of workspace ", h("strong", {}, workspaceName), ". ", useTemplate),
      field("Name", name),
      field("Program", program, "Must be on PATH, e.g. kubectl"),
      field("Arguments", args),
      h("div", { class: "field-row" }, field("Ready when output contains", readyPattern), field("Timeout (s)", timeout)),
      field("Start", startWith),
      autoRestart.element,
    );
    openModal(
      existing ? "Edit command" : "New command",
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Save",
          primary: true,
          run: async () => {
            const values = { name: name.value, program: program.value, args: args.value, readyPattern: readyPattern.value, timeout: timeout.value };
            const task = taskFromValues(id, values, startWith.value === "workspace", autoRestart.input.checked);
            saved = { config: await api.saveTask(task, options.workspaceId ?? null), task };
          },
        },
      ],
      { width: 520, onClose: () => resolve(saved) },
    );
  });
}
