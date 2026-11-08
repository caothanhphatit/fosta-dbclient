/**
 * Editors for string, hash, list, set and sorted set values. Each edit carries an `expect` guard: what the user saw of the
 * part of the key it changes, so a concurrent change fails with CONFLICT instead of being overwritten.
 */
import { formatNumber, h } from "../../core/dom";
import { toast } from "../../core/dialogs";
import { bar, button, cell, dataTable, editableCell, emptyHint, labelled, textInput, trashButton, type EditorRenderer } from "./editorKit";
import { blobBytes, blobDisplay, formatBytes, sameBlob, textBlob, type Blob, type FieldValue, type ScoredMember, type ValueRead } from "./model";

/** The window of a list, sorted set, hash or set page: the same count the value view reads the first page with. */
const PAGE = 100;

// --- String -----------------------------------------------------------------------------------------------------------------

export const stringEditor: EditorRenderer = (host, read) => {
  if (read.value.type !== "string") return emptyHint("Unexpected value");
  const data = read.value.data;
  const note = (text: string) => h("div", { class: "rd-note" }, text);
  if (data.format === "binary") {
    const bytes = blobBytes(data.value);
    const hex = [...bytes.slice(0, 2048)].map((byte) => byte.toString(16).padStart(2, "0")).join(" ");
    return h(
      "div",
      { class: "rd-editor" },
      note(`Binary value, ${formatBytes(data.length)}. Showing the first ${Math.min(bytes.length, 2048)} bytes as hex; change it from the console.`),
      h("pre", { class: "rd-pre mono" }, hex),
    );
  }
  const original = data.value;
  const area = h("textarea", { class: "rd-textarea mono", spellcheck: "false", rows: 12 });
  const status = h("span", { class: "rd-status" });
  let pretty = false;
  const rawText = blobDisplay(original);
  const show = (text: string) => {
    area.value = text;
    refresh();
  };
  const compactJson = (text: string): string => JSON.stringify(JSON.parse(text));
  const current = (): string => {
    if (!pretty) return area.value;
    return compactJson(area.value);
  };
  const dirty = (): boolean => {
    try {
      return current() !== rawText;
    } catch {
      return true;
    }
  };
  const save = button("Save", () => void commit(), "tool-button primary");
  const revert = button("Revert", () => show(pretty && data.json !== null ? JSON.stringify(data.json, null, 2) : rawText));
  const refresh = () => {
    const changed = dirty();
    save.disabled = !changed || data.truncated || host.readonly;
    revert.disabled = !changed;
    status.textContent = data.truncated
      ? `Showing the first ${formatBytes(blobBytes(original).length)} of ${formatBytes(data.length)}: too long to edit here`
      : `${formatNumber(new TextEncoder().encode(area.value).length)} bytes${changed ? " · unsaved changes" : ""}`;
  };
  const commit = async () => {
    let text: string;
    try {
      text = current();
    } catch (error) {
      toast(`This is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
      return;
    }
    await host.apply(
      { op: "setString", key: host.key, value: textBlob(text), ttlMs: null, keepTtl: true, condition: "always" },
      { kind: "string", value: original },
      "value",
    );
  };
  area.readOnly = data.truncated || host.readonly;
  area.addEventListener("input", refresh);
  const tools: HTMLElement[] = [save, revert];
  if (data.format === "json" && !data.truncated) {
    const toggle = h("label", { class: "inline-field" }, h("input", { type: "checkbox" }), h("span", {}, "Pretty JSON"));
    const box = toggle.querySelector("input") as HTMLInputElement;
    box.addEventListener("change", () => {
      try {
        const parsed = JSON.parse(area.value) as unknown;
        pretty = box.checked;
        area.value = pretty ? JSON.stringify(parsed, null, 2) : JSON.stringify(parsed);
        area.classList.toggle("pretty", pretty);
      } catch (error) {
        box.checked = pretty;
        toast(`The text is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
      }
      refresh();
    });
    tools.push(toggle);
    // A JSON string opens in the pretty view; saving writes it compact again.
    pretty = true;
    box.checked = true;
    area.value = JSON.stringify(data.json, null, 2);
    area.classList.add("pretty");
  } else {
    area.value = rawText;
  }
  refresh();
  return h("div", { class: "rd-editor" }, bar(...tools, status), area);
};

// --- Hash -------------------------------------------------------------------------------------------------------------------

export const hashEditor: EditorRenderer = (host, read) => {
  if (read.value.type !== "hash") return emptyHint("Unexpected value");
  const page = read.value.data;
  const items: FieldValue[] = [...page.items];
  let cursor = page.cursor;
  let done = page.done;
  const body = h("tbody", {});
  const more = button("Load more", () => void loadMore());
  const count = h("span", { class: "rd-status" });
  const filter = textInput("", "Filter fields (glob), Enter", "input rd-filter");
  const set = (field: Blob, value: Blob, previous: Blob | null) =>
    host.apply({ op: "hashSet", key: host.key, fields: [{ field, value }] }, { kind: "hash", fields: [{ field, value: previous }] }, "field");
  const rowOf = (item: FieldValue): HTMLElement =>
    h(
      "tr",
      {},
      cell(blobDisplay(item.field)),
      cell(editableCell(item.value, !host.readonly, (text) => void set(item.field, textBlob(text), item.value))),
      cell(
        trashButton(
          () =>
            void host.apply(
              { op: "hashDelete", key: host.key, fields: [item.field] },
              { kind: "hash", fields: [{ field: item.field, value: item.value }] },
              "field",
            ),
          "Delete this field",
          host.readonly,
        ),
      ),
    );
  const draw = () => {
    body.replaceChildren(...items.map(rowOf));
    more.hidden = done;
    count.textContent = `${formatNumber(items.length)} of ${formatNumber(page.total)} fields${filter.value ? " (filtered, the total counts all)" : ""}`;
  };
  const loadMore = async () => {
    const next = await host.api.read(host.key, { cursor, pattern: filter.value || undefined, count: PAGE });
    if (next.value.type !== "hash") return;
    items.push(...next.value.data.items);
    cursor = next.value.data.cursor;
    done = next.value.data.done;
    draw();
  };
  filter.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    void host.page({ pattern: filter.value || undefined, count: PAGE });
  });
  const name = textInput("", "New field");
  const value = textInput("", "Value");
  const add = button("Add field", () => {
    if (!name.value) return;
    void set(textBlob(name.value), textBlob(value.value), null);
  });
  draw();
  const table = h("table", { class: "rd-table" }, h("thead", {}, h("tr", {}, h("th", {}, "Field"), h("th", {}, "Value"), h("th", {}))), body);
  return h(
    "div",
    { class: "rd-editor" },
    bar(filter, count),
    table,
    h("div", { class: "rd-footer" }, more),
    host.readonly ? null : bar(labelled("Field", name), labelled("Value", value), add),
  );
};

// --- List -------------------------------------------------------------------------------------------------------------------

export const listEditor: EditorRenderer = (host, read) => {
  if (read.value.type !== "list") return emptyHint("Unexpected value");
  const page = read.value.data;
  const rows = page.items.map((item, offset) => {
    const index = page.start + offset;
    const firstOccurrence = !page.items.slice(0, offset).some((earlier) => sameBlob(earlier, item));
    return h(
      "tr",
      {},
      cell(String(index)),
      cell(
        editableCell(
          item,
          !host.readonly,
          (text) => void host.apply({ op: "listSet", key: host.key, index, value: textBlob(text) }, { kind: "listItem", index, value: item }, "item"),
        ),
      ),
      cell(
        trashButton(
          () => void host.apply({ op: "listRemove", key: host.key, value: item, count: 1 }, { kind: "listItem", index, value: item }, "item"),
          firstOccurrence ? "Remove the first element equal to this value (LREM 1)" : "An earlier element has the same value: LREM would remove that one",
          host.readonly || !firstOccurrence,
        ),
      ),
    );
  });
  const end = Math.min(page.start + page.items.length, page.total);
  const prev = button("Previous", () => void host.page({ start: Math.max(0, page.start - PAGE), count: PAGE }));
  prev.disabled = page.start === 0;
  const next = button("Next", () => void host.page({ start: page.start + PAGE, count: PAGE }));
  next.disabled = end >= page.total;
  const value = textInput("", "Value");
  const push = (position: "head" | "tail") => {
    if (!value.value) return;
    void host.apply({ op: "listPush", key: host.key, values: [textBlob(value.value)], end: position }, undefined, "list");
  };
  return h(
    "div",
    { class: "rd-editor" },
    bar(prev, next, h("span", { class: "rd-status" }, page.total === 0 ? "Empty list" : `${page.start + 1}–${end} of ${formatNumber(page.total)}`)),
    dataTable(["Index", "Value", ""], rows),
    host.readonly
      ? null
      : bar(
          labelled("Value", value),
          button("Push to head", () => push("head")),
          button("Push to tail", () => push("tail")),
        ),
  );
};

// --- Set --------------------------------------------------------------------------------------------------------------------

export const setEditor: EditorRenderer = (host, read) => {
  if (read.value.type !== "set") return emptyHint("Unexpected value");
  const page = read.value.data;
  const items: Blob[] = [...page.items];
  let cursor = page.cursor;
  let done = page.done;
  const body = h("tbody", {});
  const more = button("Load more", () => void loadMore());
  const count = h("span", { class: "rd-status" });
  const filter = textInput("", "Filter members (glob), Enter", "input rd-filter");
  const draw = () => {
    body.replaceChildren(
      ...items.map((member) =>
        h(
          "tr",
          {},
          cell(blobDisplay(member)),
          cell(
            trashButton(
              () => void host.apply({ op: "setRemove", key: host.key, members: [member] }, { kind: "set", members: [{ member, present: true }] }, "member"),
              "Remove this member",
              host.readonly,
            ),
          ),
        ),
      ),
    );
    more.hidden = done;
    count.textContent = `${formatNumber(items.length)} of ${formatNumber(page.total)} members`;
  };
  const loadMore = async () => {
    const next = await host.api.read(host.key, { cursor, pattern: filter.value || undefined, count: PAGE });
    if (next.value.type !== "set") return;
    items.push(...next.value.data.items);
    cursor = next.value.data.cursor;
    done = next.value.data.done;
    draw();
  };
  filter.addEventListener("keydown", (event) => {
    if (event.key === "Enter") void host.page({ pattern: filter.value || undefined, count: PAGE });
  });
  const member = textInput("", "New member");
  draw();
  return h(
    "div",
    { class: "rd-editor" },
    bar(filter, count),
    h("table", { class: "rd-table" }, h("thead", {}, h("tr", {}, h("th", {}, "Member"), h("th", {}))), body),
    h("div", { class: "rd-footer" }, more),
    host.readonly
      ? null
      : bar(
          labelled("Member", member),
          button("Add member", () => {
            if (!member.value) return;
            void host.apply({ op: "setAdd", key: host.key, members: [textBlob(member.value)] }, undefined, "member");
          }),
        ),
  );
};

// --- Sorted set -------------------------------------------------------------------------------------------------------------

export function zsetEditor(reverse = false): EditorRenderer {
  return (host, read: ValueRead) => {
    if (read.value.type !== "zset") return emptyHint("Unexpected value");
    const page = read.value.data;
    const request = (start: number, rev: boolean) => ({ zset: { by: "rank" as const, start, count: PAGE, rev } });
    const rows = page.items.map((item: ScoredMember, offset) => {
      const rank = page.offset + offset + 1;
      const score = h("input", { class: "input rd-cell-input rd-score", type: "text", value: String(item.score), spellcheck: "false" });
      score.readOnly = host.readonly;
      score.addEventListener("keydown", (event) => {
        if (event.key === "Enter") score.blur();
        else if (event.key === "Escape") {
          score.value = String(item.score);
          score.blur();
        }
      });
      score.addEventListener("change", () => {
        const value = Number(score.value);
        if (score.value.trim() === "" || Number.isNaN(value)) {
          toast("A score is a number (inf and -inf are allowed).");
          score.value = String(item.score);
          return;
        }
        void host.apply(
          { op: "zsetAdd", key: host.key, members: [{ member: item.member, score: value }] },
          { kind: "zset", members: [{ member: item.member, score: item.score }] },
          "score",
        );
      });
      return h(
        "tr",
        {},
        cell(String(rank)),
        cell(blobDisplay(item.member)),
        cell(score),
        cell(
          trashButton(
            () =>
              void host.apply(
                { op: "zsetRemove", key: host.key, members: [item.member] },
                { kind: "zset", members: [{ member: item.member, score: item.score }] },
                "member",
              ),
            "Remove this member",
            host.readonly,
          ),
        ),
      );
    });
    const end = page.offset + page.items.length;
    const prev = button("Previous", () => void host.page(request(Math.max(0, page.offset - PAGE), reverse)));
    prev.disabled = page.offset === 0;
    const next = button("Next", () => void host.page(request(page.offset + PAGE, reverse)));
    next.disabled = end >= page.total;
    const order = button(reverse ? "Lowest first" : "Highest first", () => void host.page(request(0, !reverse)));
    const member = textInput("", "New member");
    const scoreInput = textInput("", "Score");
    const add = button("Add member", () => {
      const value = Number(scoreInput.value);
      if (!member.value || scoreInput.value.trim() === "" || Number.isNaN(value)) {
        toast("Enter a member and a numeric score.");
        return;
      }
      void host.apply(
        { op: "zsetAdd", key: host.key, members: [{ member: textBlob(member.value), score: value }] },
        { kind: "zset", members: [{ member: textBlob(member.value), score: null }] },
        "member",
      );
    });
    return h(
      "div",
      { class: "rd-editor" },
      bar(
        prev,
        next,
        order,
        h("span", { class: "rd-status" }, page.total === 0 ? "Empty sorted set" : `Ranks ${page.offset + 1}–${end} of ${formatNumber(page.total)}`),
      ),
      h(
        "table",
        { class: "rd-table" },
        h("thead", {}, h("tr", {}, h("th", {}, "Rank"), h("th", {}, "Member"), h("th", {}, "Score"), h("th", {}))),
        h("tbody", {}, ...rows),
      ),
      host.readonly ? null : bar(labelled("Member", member), labelled("Score", scoreInput), add),
    );
  };
}
