/** The stream editor: entries newest first with paging, the consumer groups, and an append form (XADD). */
import { formatNumber, h } from "../../core/dom";
import { toast } from "../../core/dialogs";
import { bar, button, cell, dataTable, emptyHint, labelled, textInput, type EditorRenderer } from "./editorKit";
import { blobDisplay, textBlob, type FieldValue, type StreamEntry } from "./model";

const PAGE = 50;

/** `*` or empty for an automatic id, else `ms` or `ms-seq`. */
export function validStreamId(text: string): boolean {
  return text === "" || text === "*" || /^\d+(-(\d+|\*))?$/.test(text);
}

function fieldsText(entry: StreamEntry): HTMLElement {
  return h(
    "div",
    { class: "rd-fields" },
    ...entry.fields.map((field) => h("span", { class: "rd-field" }, h("b", {}, blobDisplay(field.field)), " ", blobDisplay(field.value))),
  );
}

export const streamEditor: EditorRenderer = (host, read) => {
  if (read.value.type !== "stream") return emptyHint("Unexpected value");
  const { page, info } = read.value.data;
  const entries: StreamEntry[] = [...page.entries];
  let next = page.next;
  const body = h("tbody", {});
  const more = button("Load older entries", () => void loadOlder());
  const draw = () => {
    body.replaceChildren(...entries.map((entry) => h("tr", {}, cell(h("span", { class: "mono" }, entry.id)), cell(fieldsText(entry)))));
    more.hidden = next === null;
  };
  const loadOlder = async () => {
    const result = await host.api.read(host.key, { stream: { before: next, count: PAGE, reverse: true } });
    if (result.value.type !== "stream") return;
    entries.push(...result.value.data.page.entries);
    next = result.value.data.page.next;
    draw();
  };
  draw();
  const summary = h(
    "div",
    { class: "rd-summary" },
    h("span", {}, `${formatNumber(info.length)} entries`),
    info.firstId ? h("span", {}, `first ${info.firstId}`) : null,
    info.lastId ? h("span", {}, `last ${info.lastId}`) : null,
  );
  const groups =
    info.groups.length === 0
      ? null
      : dataTable(
          ["Group", "Consumers", "Pending", "Last delivered", "Lag"],
          info.groups.map((group) =>
            h(
              "tr",
              {},
              cell(group.name),
              cell(String(group.consumers)),
              cell(String(group.pending)),
              cell(h("span", { class: "mono" }, group.lastDeliveredId)),
              cell(group.lag === null ? "" : String(group.lag)),
            ),
          ),
        );
  const entriesTable = h("table", { class: "rd-table" }, h("thead", {}, h("tr", {}, h("th", {}, "Id"), h("th", {}, "Fields"))), body);
  const form = host.readonly ? null : appendForm(host);
  return h(
    "div",
    { class: "rd-editor" },
    summary,
    groups ? h("div", { class: "section-title" }, "Consumer groups") : null,
    groups,
    h("div", { class: "section-title" }, "Entries, newest first"),
    entriesTable,
    h("div", { class: "rd-footer" }, more),
    form,
  );
};

function appendForm(host: Parameters<EditorRenderer>[0]): HTMLElement {
  const id = textInput("", "* (automatic)", "input rd-id");
  const maxLen = textInput("", "optional", "input rd-id");
  const rows = h("div", { class: "rd-field-rows" });
  const addRow = (name = "", value = "") => {
    const row = h("div", { class: "rd-field-row" }, textInput(name, "Field"), textInput(value, "Value"));
    rows.append(row);
  };
  addRow();
  const submit = button(
    "Append entry",
    () => {
      const fields: FieldValue[] = [];
      for (const row of rows.querySelectorAll<HTMLElement>(".rd-field-row")) {
        const [name, value] = [...row.querySelectorAll("input")].map((input) => input.value);
        if (name) fields.push({ field: textBlob(name), value: textBlob(value ?? "") });
      }
      if (fields.length === 0) {
        toast("An entry needs at least one field with a name.");
        return;
      }
      if (!validStreamId(id.value.trim())) {
        toast("An entry id is `*`, a millisecond time, or `time-sequence` such as 1712345678901-0.");
        return;
      }
      const limit = maxLen.value.trim() === "" ? null : Number(maxLen.value);
      if (limit !== null && (!Number.isInteger(limit) || limit < 0)) {
        toast("The maximum length is a whole number.");
        return;
      }
      const text = id.value.trim();
      void host.apply({ op: "streamAdd", key: host.key, id: text === "" || text === "*" ? null : text, fields, maxLen: limit }, undefined, "stream");
    },
    "tool-button primary",
  );
  return h(
    "div",
    { class: "rd-append" },
    h("div", { class: "section-title" }, "Append an entry"),
    bar(labelled("Id", id), labelled("Trim to", maxLen)),
    rows,
    bar(
      button("Add field", () => addRow()),
      submit,
    ),
  );
}
