import { api } from "../../core/api";
import { checkbox, field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import { formatArgs, parseArgs } from "../../core/shell";
import type { AppConfig, TaskDef } from "../../core/types";

const PORT_FORWARD_TEMPLATE = {
  name: "MariaDB port-forward",
  program: "kubectl",
  args: "port-forward -n default svc/mariadb 13306:3306",
  readyPattern: "Forwarding from",
};

export function taskDialog(existing: TaskDef | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "");
    const program = textInput(existing?.program ?? "kubectl", { class: "input mono" });
    const args = textInput(formatArgs(existing?.args ?? []), {
      class: "input mono",
      placeholder: "port-forward -n <namespace> svc/<service> <local>:<remote>",
    });
    const readyPattern = textInput(existing?.readyPattern ?? "", { placeholder: "text in output that means ready" });
    const autoStart = checkbox("Start automatically when this workspace opens", existing?.autoStart ?? true);
    const autoRestart = checkbox("Restart automatically when the process exits", existing?.autoRestart ?? true);
    const useTemplate = h("button", { class: "link-button", type: "button" }, "Fill kubectl port-forward template");
    useTemplate.addEventListener("click", () => {
      name.value ||= PORT_FORWARD_TEMPLATE.name;
      program.value = PORT_FORWARD_TEMPLATE.program;
      args.value = PORT_FORWARD_TEMPLATE.args;
      readyPattern.value = PORT_FORWARD_TEMPLATE.readyPattern;
    });

    const readTask = (): TaskDef => {
      const command = program.value.trim();
      if (!command) throw new Error("Program is required");
      return {
        id,
        name: name.value.trim() || command,
        program: command,
        args: parseArgs(args.value),
        autoStart: autoStart.input.checked,
        autoRestart: autoRestart.input.checked,
        readyPattern: readyPattern.value.trim() || null,
      };
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Runs in the background while workspace ", h("strong", {}, workspaceName), " is open. ", useTemplate),
      field("Task name", name),
      field("Program", program, "Must be on PATH, e.g. kubectl"),
      field("Arguments", args),
      field("Ready when output contains", readyPattern),
      autoStart.element,
      autoRestart.element,
    );
    openModal(
      existing ? "Edit startup task" : "New startup task",
      body,
      [
        { label: "Cancel", run: () => undefined },
        { label: "Save", primary: true, run: async () => void (saved = await api.saveTask(readTask())) },
      ],
      { width: 520, onClose: () => resolve(saved) },
    );
  });
}
