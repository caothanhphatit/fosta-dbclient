/**
 * The view of one key: its type, TTL (set and remove), size and a typed editor. Every edit is guarded by what the user last
 * saw (`expect`); a concurrent change shows the conflict with "Reload" and "Overwrite" instead of silently winning.
 */
import { toast } from "../../core/dialogs";
import { copyText, h } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { button, emptyHint, textInput, type EditorHost, type EditorRenderer } from "./editorKit";
import {
  EDITABLE_TYPES,
  blobDisplay,
  conflictOf,
  describeCurrent,
  formatBytes,
  formatTtl,
  keyBlob,
  parseTtl,
  typeName,
  type Blob,
  type Expect,
  type ReadOptions,
  type RedisEdit,
  type ValueRead,
} from "./model";
import { RedisApi } from "./redisApi";
import { streamEditor } from "./streamEditor";
import { hashEditor, listEditor, setEditor, stringEditor, zsetEditor } from "./valueEditors";
import "./redis.css";

const EDITORS: Record<string, EditorRenderer> = {
  string: stringEditor,
  hash: hashEditor,
  list: listEditor,
  set: setEditor,
  zset: zsetEditor(false),
  stream: streamEditor,
};

/** Highest-first sorted sets use the same editor with the order flipped. */
const REVERSED_ZSET = zsetEditor(true);

/** The key an object reference stands for, and its database. */
export function targetOf(ctx: ViewContext): { key: Blob; db: number } {
  const object = ctx.object;
  if (!object) throw new Error("Open a key from the tree or the key list.");
  return { key: keyBlob(object.name, object.binary === true), db: Number(object.database ?? 0) };
}

export class ValueView implements TabContent {
  readonly element: HTMLElement;
  private readonly api: RedisApi;
  private readonly key: Blob;
  private readonly readonly: boolean;
  private readonly head = h("div", { class: "rd-head" });
  private readonly meta = h("div", { class: "rd-meta" });
  private readonly banner = h("div", { class: "rd-banner", hidden: true });
  private readonly body = h("div", { class: "rd-body" });
  private options: ReadOptions = { count: 100, stream: { reverse: true, count: 50 } };
  private reversedZset = false;
  private disposed = false;

  constructor(ctx: ViewContext) {
    const target = targetOf(ctx);
    this.key = target.key;
    this.api = new RedisApi(ctx.connection, target.db);
    this.readonly = ctx.navigator.writesBlocked(ctx.connection.connectionId);
    this.element = h("div", { class: "view rd-value" }, this.head, this.meta, this.banner, this.body);
    void this.load();
  }

  dispose(): void {
    this.disposed = true;
  }

  onActivate(): void {
    // The key may have changed while the tab was in the background; the user reloads explicitly (the tab keeps its edits).
  }

  private host(): EditorHost {
    return {
      key: this.key,
      api: this.api,
      readonly: this.readonly,
      apply: (edit, expect, label) => this.apply(edit, expect, label),
      page: async (options) => {
        this.options = { ...this.options, ...options };
        if (options.zset) this.reversedZset = options.zset.rev;
        await this.load(false);
      },
    };
  }

  async load(resetPaging = true): Promise<void> {
    if (resetPaging) {
      this.options = { count: 100, stream: { reverse: true, count: 50 } };
      this.reversedZset = false;
    }
    try {
      const read = await this.api.read(this.key, this.options);
      if (this.disposed) return;
      this.render(read);
    } catch (error) {
      if (this.disposed) return;
      this.body.replaceChildren(emptyHint("The key could not be read."));
      showError(error, `Read ${blobDisplay(this.key)}`);
    }
  }

  /** Runs an edit with its guard; shows a conflict or an error and resolves false when it did not apply. */
  private async apply(edit: RedisEdit, expect: Expect | undefined, label: string): Promise<boolean> {
    this.banner.hidden = true;
    try {
      const result = await this.api.edit(edit, expect);
      if (result === null) return false;
      if (!result.applied) toast("Nothing changed: the server did not apply it.");
      await this.load(false);
      return result.applied;
    } catch (error) {
      const conflict = conflictOf(error);
      if (conflict) {
        this.showConflict(describeCurrent(conflict), label, edit);
      } else {
        showError(error, `Save the ${label} of ${blobDisplay(this.key)}`);
      }
      return false;
    }
  }

  private showConflict(current: string, label: string, edit: RedisEdit): void {
    const overwrite = button("Overwrite", () => {
      this.banner.hidden = true;
      void this.apply(edit, undefined, label);
    });
    this.banner.replaceChildren(
      h("strong", {}, `Someone else changed this ${label}. `),
      h("span", {}, `${current} Nothing was saved.`),
      h(
        "span",
        { class: "rd-banner-actions" },
        button("Reload", () => void this.reload()),
        overwrite,
      ),
    );
    this.banner.hidden = false;
  }

  private async reload(): Promise<void> {
    this.banner.hidden = true;
    await this.load(false);
  }

  private render(read: ValueRead): void {
    const info = read.info;
    const type = typeName(info.keyType);
    const present = <T extends Node>(items: (T | null)[]): T[] => items.filter((item): item is T => item !== null);
    this.head.replaceChildren(
      h("span", { class: `rd-type type-${type}` }, type || "key"),
      h("span", { class: "rd-keyname mono", title: blobDisplay(this.key) }, blobDisplay(this.key)),
      h("span", { class: "spacer" }),
      button("Copy key", () => void copyText(blobDisplay(this.key)).then(() => toast("Key copied", "success"))),
      button("Reload", () => void this.reload()),
      ...present([this.readonly || read.value.type === "none" ? null : button("Delete key", () => void this.deleteKey(), "tool-button danger")]),
    );
    this.meta.replaceChildren(
      h("span", {}, `db${this.api.db}`),
      ...present<HTMLElement>([
        info.length === null ? null : h("span", {}, `${info.length.toLocaleString()} ${info.keyType === "string" ? "bytes" : "elements"}`),
        info.memoryBytes === null ? null : h("span", { title: "MEMORY USAGE (sampled)" }, formatBytes(info.memoryBytes)),
        info.encoding ? h("span", { title: "OBJECT ENCODING" }, info.encoding) : null,
      ]),
      this.ttlControl(info.ttlMs),
    );
    this.body.replaceChildren(this.editorFor(read));
  }

  private ttlControl(ttlMs: number): HTMLElement {
    const input = textInput("", "10m, 2h, 3600", "input rd-ttl-input");
    const apply = button("Set TTL", () => {
      const ms = parseTtl(input.value);
      if (ms === null) {
        toast("A TTL is a number with an optional unit: 90, 90s, 10m, 2h, 1d.");
        return;
      }
      void this.apply({ op: "expire", key: this.key, ttlMs: ms }, undefined, "TTL");
    });
    const persist = button("Remove TTL", () => void this.apply({ op: "persist", key: this.key }, undefined, "TTL"));
    persist.disabled = ttlMs < 0;
    if (this.readonly) {
      input.disabled = true;
      apply.disabled = true;
      persist.disabled = true;
    }
    return h("span", { class: "rd-ttl" }, h("span", { class: "rd-ttl-value" }, `TTL ${formatTtl(ttlMs)}`), input, apply, persist);
  }

  private editorFor(read: ValueRead): HTMLElement {
    const value = read.value;
    switch (value.type) {
      case "none":
        return emptyHint("This key does not exist (any more). It may have expired or been deleted.");
      case "json":
        return h(
          "div",
          { class: "rd-editor" },
          h("div", { class: "rd-note" }, "A RedisJSON document: shown read-only here; change it with JSON.SET in the console."),
          h("pre", { class: "rd-pre mono" }, JSON.stringify(value.data.value, null, 2)),
        );
      case "unsupported":
        return emptyHint(`Keys of type ${value.data.typeName} have no editor yet. Use the console to work with them.`);
      default: {
        const type = typeName(read.info.keyType);
        if (!EDITABLE_TYPES.has(type)) return emptyHint(`Keys of type ${type} have no editor.`);
        const renderer = type === "zset" && this.reversedZset ? REVERSED_ZSET : EDITORS[type];
        return renderer(this.host(), read);
      }
    }
  }

  private async deleteKey(): Promise<void> {
    try {
      const result = await this.api.edit({ op: "delete", keys: [this.key] });
      if (!result) return;
      toast(result.count === 0 ? "The key was already gone" : "Key deleted", "success");
      await this.load(false);
    } catch (error) {
      showError(error, `Delete ${blobDisplay(this.key)}`);
    }
  }
}

export const valueViewFactory = (ctx: ViewContext): ValueView => new ValueView(ctx);
