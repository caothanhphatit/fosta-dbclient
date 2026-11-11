/**
 * The code export dialog: a query or a pipeline as mongosh, Node.js or Python code (`codegen.ts`), with a language
 * switch and Copy. The language picked last is offered first next time (in this window).
 */
import { openModal, toast } from "../../core/dialogs";
import { copyText, h } from "../../core/dom";
import { CODE_LANGUAGES, type CodegenResult, type CodeLanguage } from "./codegen";

let lastLanguage: CodeLanguage = "mongosh";

/** Opens the dialog; `generate` writes the code in a language (or says why it cannot). */
export function openCodeExport(title: string, generate: (language: CodeLanguage) => CodegenResult): void {
  let language = lastLanguage;
  let code: string | null = null;
  const output = h("pre", { class: "ag-code", tabindex: "0", "aria-label": "Code" });
  const buttons = new Map<CodeLanguage, HTMLButtonElement>();
  const switcher = h("div", { class: "segmented ag-code-languages", role: "group", "aria-label": "Language" });
  const show = () => {
    for (const [id, button] of buttons) {
      button.classList.toggle("active", id === language);
      button.setAttribute("aria-pressed", String(id === language));
    }
    const result = generate(language);
    code = result.ok ? result.code : null;
    output.classList.toggle("ag-code-error", !result.ok);
    output.textContent = result.ok ? result.code : `This cannot be written as code: ${result.error}`;
  };
  for (const item of CODE_LANGUAGES) {
    const button = h("button", { class: "segment", type: "button", "aria-pressed": "false" }, item.label);
    button.addEventListener("click", () => {
      language = item.id;
      lastLanguage = item.id;
      show();
    });
    buttons.set(item.id, button);
    switcher.append(button);
  }
  show();
  openModal(
    title,
    h("div", { class: "ag-code-body" }, switcher, output),
    [
      { label: "Close", run: () => undefined },
      {
        label: "Copy",
        primary: true,
        run: async () => {
          if (code === null) return false;
          try {
            await copyText(code);
            toast("Code copied", "success");
          } catch {
            toast("Could not write to the clipboard", "error");
            return false;
          }
        },
      },
    ],
    { width: 720 },
  );
}
