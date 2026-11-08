/**
 * The key browser of one database: `SCAN` with a pattern and an optional type filter (never `KEYS`), paged by the server's
 * cursor, with the type, TTL and (when asked) memory of each key. Opening a key opens its value view.
 */
import { toast } from "../../core/dialogs";
import { formatNumber, h, icon } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { ObjectRef } from "../../core/contract";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { button, emptyHint, labelled, textInput } from "./editorKit";
import {
  blobDisplay,
  conflictOf,
  formatBytes,
  formatTtl,
  keyIdentity,
  matchPattern,
  splitKey,
  textBlob,
  typeName,
  type Expect,
  type KeyRow,
  type RedisEdit,
} from "./model";
import { RedisApi } from "./redisApi";
import "./redis.css";

const PAGE = 200;
const TYPES = ["", "string", "hash", "list", "set", "zset", "stream"];

/** The edit that creates a key of `type` with one initial element; the caller guards it with `{ kind: "missing" }`. */
export function creation(type: string, key: string, first: string, second: string): RedisEdit | string {
  const k = textBlob(key);
  switch (type) {
    case "string":
      return { op: "setString", key: k, value: textBlob(first), ttlMs: null, keepTtl: false, condition: "ifMissing" };
    case "hash":
      return first ? { op: "hashSet", key: k, fields: [{ field: textBlob(first), value: textBlob(second) }] } : "Enter a field name.";
    case "list":
      return { op: "listPush", key: k, values: [textBlob(first)], end: "tail" };
    case "set":
      return { op: "setAdd", key: k, members: [textBlob(first)] };
    case "zset": {
      const score = Number(second);
      return first && second.trim() !== "" && !Number.isNaN(score)
        ? { op: "zsetAdd", key: k, members: [{ member: textBlob(first), score }] }
        : "Enter a member and a numeric score.";
    }
    case "stream":
      return first ? { op: "streamAdd", key: k, id: null, fields: [{ field: textBlob(first), value: textBlob(second) }], maxLen: null } : "Enter a field name.";
    default:
      return "Pick a type.";
  }
}

export class KeysView implements TabContent {
  readonly element: HTMLElement;
  private readonly api: RedisApi;
  private readonly readonly: boolean;
  private readonly pattern = textInput("", "Pattern, e.g. user:*  (plain text matches anywhere)", "input rd-pattern");
  private readonly typeSelect = h("select", { class: "input" }, ...TYPES.map((type) => h("option", { value: type }, type === "" ? "any type" : type)));
  private readonly memory = h("input", { type: "checkbox" });
  private readonly body = h("tbody", {});
  private readonly status = h("span", { class: "rd-status" });
  private readonly more = button("Load more", () => void this.scan(false));
  private readonly create = h("div", { class: "rd-create", hidden: true });
  private rows: KeyRow[] = [];
  private cursor = "";
  private done = true;
  private dbSize: number | null = null;
  private generation = 0;
  private disposed = false;

  constructor(private readonly ctx: ViewContext) {
    const db = Number(ctx.object?.database ?? 0);
    this.api = new RedisApi(ctx.connection, db);
    this.readonly = ctx.navigator.writesBlocked(ctx.connection.connectionId);
    this.pattern.addEventListener("keydown", (event) => {
      if (event.key === "Enter") void this.scan(true);
    });
    this.typeSelect.addEventListener("change", () => void this.scan(true));
    this.memory.addEventListener("change", () => void this.scan(true));
    const toolbar = h(
      "div",
      { class: "toolbar rd-toolbar" },
      button("Scan", () => void this.scan(true), "tool-button primary", "Run SCAN with this pattern from the start"),
      this.pattern,
      labelled("Type", this.typeSelect),
      h("label", { class: "inline-field", title: "MEMORY USAGE for the keys of each page (sampled)" }, this.memory, h("span", {}, "Memory")),
      this.readonly ? null : button("New key", () => this.toggleCreate()),
      h("span", { class: "spacer" }),
      this.status,
    );
    const table = h(
      "table",
      { class: "rd-table rd-keys" },
      h("thead", {}, h("tr", {}, h("th", {}, "Type"), h("th", {}, "Key"), h("th", {}, "TTL"), h("th", { class: "rd-memory-col" }, "Memory"))),
      this.body,
    );
    this.element = h(
      "div",
      { class: "view rd-keys-view" },
      toolbar,
      this.create,
      h("div", { class: "rd-scroll" }, table, h("div", { class: "rd-footer" }, this.more)),
    );
    void this.scan(true);
  }

  dispose(): void {
    this.disposed = true;
  }

  private async scan(fresh: boolean): Promise<void> {
    const generation = fresh ? ++this.generation : this.generation;
    if (fresh) {
      this.rows = [];
      this.cursor = "";
      this.body.replaceChildren();
    }
    this.more.disabled = true;
    this.status.textContent = "Scanning…";
    try {
      const page = await this.api.scan({
        cursor: this.cursor,
        pattern: matchPattern(this.pattern.value),
        typeFilter: this.typeSelect.value || undefined,
        pageSize: PAGE,
        withMemory: this.memory.checked,
      });
      if (this.disposed || generation !== this.generation) return;
      this.rows.push(...page.keys);
      this.cursor = page.cursor;
      this.done = page.done;
      this.dbSize = page.dbSize;
      this.draw();
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.status.textContent = "";
      showError(error, `Scan db${this.api.db}`);
    } finally {
      this.more.disabled = false;
    }
  }

  private draw(): void {
    this.element.classList.toggle("with-memory", this.memory.checked);
    this.body.replaceChildren(...this.rows.map((row) => this.rowOf(row)));
    if (this.rows.length === 0)
      this.body.replaceChildren(h("tr", {}, h("td", { colspan: 4 }, emptyHint(this.done ? "No keys match." : "No keys in this page yet: load more."))));
    this.more.hidden = this.done;
    const total = this.dbSize === null ? "" : ` of ${formatNumber(this.dbSize)} in db${this.api.db}`;
    this.status.textContent = `${formatNumber(this.rows.length)} keys${total}${this.done ? "" : " · more to scan"}`;
  }

  private rowOf(row: KeyRow): HTMLElement {
    const name = blobDisplay(row.key);
    const { prefix, leaf } = splitKey(name);
    const type = typeName(row.keyType);
    const ttl = row.ttlMs === null || row.ttlMs === -1 ? "" : formatTtl(row.ttlMs);
    const open = () => this.openKey(row);
    const tr = h(
      "tr",
      { class: "rd-key-row", tabindex: "0", title: name },
      h("td", {}, h("span", { class: `rd-type type-${type}` }, type)),
      h("td", { class: "mono" }, prefix ? h("span", { class: "rd-prefix" }, prefix) : null, leaf),
      h("td", { class: "rd-ttl-cell" }, ttl),
      h("td", { class: "rd-memory-col" }, formatBytes(row.memoryBytes)),
    );
    tr.addEventListener("dblclick", open);
    tr.addEventListener("click", open);
    tr.addEventListener("keydown", (event) => {
      if (event.key === "Enter") open();
    });
    return tr;
  }

  private openKey(row: KeyRow): void {
    const { name, binary } = keyIdentity(row.key);
    const object: ObjectRef = { database: String(this.api.db), schema: null, name, kind: "key", ...(binary ? { binary } : {}) };
    this.ctx.navigator.openData(this.ctx.connection.connectionId, object, {});
  }

  private toggleCreate(): void {
    if (!this.create.hidden) {
      this.create.hidden = true;
      return;
    }
    const type = h("select", { class: "input" }, ...TYPES.filter(Boolean).map((item) => h("option", { value: item }, item)));
    const key = textInput("", "Key name");
    const first = textInput("", "Value");
    const second = textInput("", "");
    const hint = () => {
      const labels: Record<string, [string, string]> = {
        string: ["Value", ""],
        hash: ["Field", "Value"],
        list: ["First element", ""],
        set: ["First member", ""],
        zset: ["Member", "Score"],
        stream: ["Field", "Value"],
      };
      const [one, two] = labels[type.value] ?? ["Value", ""];
      first.placeholder = one;
      second.placeholder = two;
      second.hidden = two === "";
    };
    type.addEventListener("change", hint);
    hint();
    const submit = button(
      "Create",
      () => {
        if (!key.value) {
          toast("Enter a key name.");
          return;
        }
        const edit = creation(type.value, key.value, first.value, second.value);
        if (typeof edit === "string") {
          toast(edit);
          return;
        }
        const expect: Expect = { kind: "missing" };
        void this.api
          .edit(edit, expect)
          .then((result) => {
            if (result === null) return;
            toast(`Created ${key.value}`, "success");
            this.create.hidden = true;
            this.pattern.value = key.value;
            return this.scan(true);
          })
          .catch((error: unknown) => (conflictOf(error) ? toast(`A key named ${key.value} already exists.`) : showError(error, `Create ${key.value}`)));
      },
      "tool-button primary",
    );
    this.create.replaceChildren(
      icon("plus"),
      labelled("Type", type),
      key,
      first,
      second,
      submit,
      button("Cancel", () => (this.create.hidden = true)),
    );
    this.create.hidden = false;
    key.focus();
  }
}

export const keysViewFactory = (ctx: ViewContext): KeysView => new KeysView(ctx);
