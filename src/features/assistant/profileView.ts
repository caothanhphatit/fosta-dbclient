/**
 * "What the assistant knows": a tab over the stored profile of one database. The header says when it was learned, with
 * which model and tokens, whether row values were read and whether the schema changed since (Refresh, Refresh changed
 * only, Delete). Below: the model's summary, the user's notes (database-wide or about one object), and the searchable
 * object list with signatures, descriptions (editable; the user's are marked), columns with value hints and relations.
 */
import { api, errorText } from "../../core/api";
import { confirmDialog, toast } from "../../core/dialogs";
import { h, icon, spacer, toolButton } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { DatabaseProfile, ObjectProfile, ProfileNote, ProfileStatus } from "../../core/profileTypes";
import type { TabContent } from "../../core/tabs";
import type { LearnTarget } from "./learnDialog";
import { columnSignature, editedNote, formatTokens, formatWhen, hintText, qualifiedName, relationsOf, searchObjects } from "./profileLogic";

export interface ProfileViewHost {
  /** Opens the learn dialog (a refresh when a profile exists); resolves once it closed. */
  learn(target: LearnTarget, onlyChanged: boolean): Promise<{ learned: boolean }>;
  /** The profile was deleted, refreshed or annotated (the assistant panel re-reads its status). */
  changed(target: LearnTarget): void;
  /** The tab checked the profile's status (the panel shows the same stale badge). */
  statusSeen(target: LearnTarget, status: ProfileStatus): void;
}

/** Objects drawn before "Show more". */
const PAGE = 100;

export class ProfileView implements TabContent {
  readonly element: HTMLElement;
  private readonly header = h("div", { class: "knows-header" });
  private readonly content = h("div", { class: "knows-content" });
  private readonly search = h("input", {
    class: "input knows-search",
    type: "search",
    placeholder: "Search objects, columns, descriptions…",
    spellcheck: "false",
  });
  private readonly objectList = h("div", { class: "knows-objects" });
  private readonly objectCount = h("span", { class: "knows-count" });
  private profile: DatabaseProfile | null = null;
  private status: ProfileStatus | null = null;
  private shown = PAGE;
  private loading = false;

  constructor(
    readonly target: LearnTarget,
    private readonly host: ProfileViewHost,
  ) {
    const toolbar = h(
      "div",
      { class: "toolbar" },
      icon("book", "toolbar-icon"),
      h("span", { class: "toolbar-title" }, "What the assistant knows"),
      spacer(),
      toolButton("refresh", "Refresh", () => void this.relearn(false), "Learn the structure again and, if you choose, summarise it with the model"),
      toolButton("bolt", "Refresh changed only", () => void this.relearn(true), "Learn the structure again; the model describes only new or changed objects"),
      toolButton("trash", "Delete", () => void this.remove(), "Delete what the assistant learned about this database"),
    );
    this.search.addEventListener("input", () => {
      this.shown = PAGE;
      this.renderObjects();
    });
    this.element = h("div", { class: "view knows-view" }, toolbar, h("div", { class: "knows-scroll" }, this.header, this.content));
    void this.load();
  }

  onActivate(): void {
    if (!this.loading) void this.load();
  }

  private async load(force = false): Promise<void> {
    this.loading = true;
    try {
      const [profile, status] = await Promise.all([
        api.profileGet(this.target.connectionId, this.target.database),
        api.profileStatus(this.target.connectionId, this.target.database, force).catch(() => null),
      ]);
      this.profile = profile;
      this.status = status;
      if (status) this.host.statusSeen(this.target, status);
      this.render();
    } catch (error) {
      this.header.replaceChildren();
      this.content.replaceChildren(h("p", { class: "learn-error" }, errorText(error)));
    } finally {
      this.loading = false;
    }
  }

  private async relearn(onlyChanged: boolean): Promise<void> {
    const result = await this.host.learn(this.target, onlyChanged && this.profile !== null);
    if (result.learned) {
      this.host.changed(this.target);
      await this.load(true);
    }
  }

  private async remove(): Promise<void> {
    if (!this.profile) return;
    const name = this.target.database || this.target.connectionName;
    if (!(await confirmDialog(`Delete what the assistant learned about ${name}? Your notes and descriptions go too. This cannot be undone.`, "Delete"))) return;
    try {
      await api.profileDelete(this.target.connectionId, this.target.database);
      toast(`Deleted what the assistant learned about ${name}`);
      this.host.changed(this.target);
      await this.load();
    } catch (error) {
      showError(error, "Delete what the assistant learned");
    }
  }

  // --- Rendering ---------------------------------------------------------------------------------------------------------

  private render(): void {
    const profile = this.profile;
    if (!profile) {
      const learn = h("button", { class: "btn primary", type: "button" }, "Learn this database");
      learn.addEventListener("click", () => void this.relearn(false));
      this.header.replaceChildren();
      this.content.replaceChildren(
        h(
          "div",
          { class: "knows-empty" },
          icon("book", "knows-empty-icon"),
          h("h3", {}, "The assistant has not learned this database yet"),
          h(
            "p",
            {},
            "Learning saves the structure (and, where you allow it, a few sample values) on this computer, so the assistant knows the database in every chat.",
          ),
          learn,
        ),
      );
      return;
    }
    this.header.replaceChildren(...this.headerFacts(profile));
    this.content.replaceChildren(this.summarySection(profile), this.notesSection(), this.objectsSection());
    this.renderObjects();
  }

  private headerFacts(profile: DatabaseProfile): HTMLElement[] {
    const fact = (label: string, value: string, title = "") =>
      h("span", { class: "knows-fact", title }, h("span", { class: "knows-fact-label" }, label), value);
    const where = this.target.database ? `${this.target.connectionName} · ${this.target.database}` : this.target.connectionName;
    const facts = [
      h("span", { class: "knows-fact knows-where" }, where),
      fact("Learned", formatWhen(profile.learnedAt), new Date(profile.learnedAt).toLocaleString()),
      fact("Objects", profile.truncated ? `${profile.objects.length} of ${profile.totalObjects}` : String(profile.totalObjects)),
      fact("Relations", String(profile.relations.length)),
      fact("Model", profile.model ?? "none yet"),
      fact(
        "Tokens",
        profile.inputTokens || profile.outputTokens ? `~${formatTokens(profile.inputTokens)} in, ~${formatTokens(profile.outputTokens)} out` : "0",
      ),
      fact("Data read", profile.dataRead ? "yes (value hints)" : "no (structure only)"),
    ];
    const stale = this.status?.stale;
    const badge =
      stale === true
        ? staleBadge(() => void this.relearn(true))
        : h(
            "span",
            { class: `knows-state${stale === false ? " ok" : ""}` },
            stale === false ? "Up to date" : "Not checked (connect to check for schema changes)",
          );
    return [h("div", { class: "knows-facts" }, ...facts, badge)];
  }

  private summarySection(profile: DatabaseProfile): HTMLElement {
    const text = profile.summary?.text;
    return section(
      "Summary",
      text
        ? h("p", { class: "knows-summary" }, text)
        : h("p", { class: "knows-muted" }, "No summary yet. Refresh and choose Summarise to have the model write one and describe the objects."),
      profile.summary ? h("span", { class: "knows-muted knows-small" }, `${profile.summary.model} · ${formatWhen(profile.summary.createdAt)}`) : null,
    );
  }

  // --- Notes ---------------------------------------------------------------------------------------------------------------

  private notesSection(): HTMLElement {
    const list = h("div", { class: "knows-notes" });
    const render = () => {
      list.replaceChildren(
        ...(this.profile?.notes.length ? this.profile.notes.map((note) => this.noteRow(note, render)) : [h("p", { class: "knows-muted" }, "No notes yet.")]),
      );
    };
    render();
    const add = h("button", { class: "btn small", type: "button" }, icon("plus"), "Add note");
    add.addEventListener("click", () => {
      add.hidden = true;
      focusEditor(
        list,
        this.noteEditor(null, () => {
          add.hidden = false;
          render();
        }),
        "append",
      );
    });
    return section(
      "Notes",
      h(
        "p",
        { class: "knows-muted knows-small" },
        "Your own knowledge (terms, rules, what a status means). The assistant reads it in every chat; learning again never changes it.",
      ),
      list,
      add,
    );
  }

  private noteRow(note: ProfileNote, rerender: () => void): HTMLElement {
    const row = h(
      "div",
      { class: "knows-note" },
      h("span", { class: "knows-note-scope" }, note.object ? note.object : "Whole database"),
      h("span", { class: "knows-note-text" }, note.text),
    );
    const edit = toolButton("edit", "Edit", () => focusEditor(row, this.noteEditor(note, rerender), "replace"));
    const remove = toolButton("trash", "Delete", () => void this.saveNotes((this.profile?.notes ?? []).filter((item) => item.id !== note.id)).then(rerender));
    row.append(h("span", { class: "knows-note-actions" }, edit, remove));
    return row;
  }

  private noteEditor(note: ProfileNote | null, done: () => void): HTMLElement {
    const text = h("textarea", {
      class: "input knows-note-input",
      rows: "2",
      placeholder: "e.g. status 3 means refunded; amounts are in cents",
      spellcheck: "true",
    });
    text.value = note?.text ?? "";
    const scope = h(
      "select",
      { class: "input knows-note-object" },
      h("option", { value: "" }, "Whole database"),
      ...(this.profile?.objects ?? []).map((object) => h("option", { value: qualifiedName(object) }, qualifiedName(object))),
    );
    scope.value = note?.object ?? "";
    const save = h("button", { class: "btn small primary", type: "button" }, "Save");
    const cancel = h("button", { class: "btn small", type: "button" }, "Cancel");
    save.addEventListener("click", () => {
      const value = text.value.trim();
      if (!value) {
        text.focus();
        return;
      }
      const notes = this.profile?.notes ?? [];
      const next = editedNote(note, value, scope.value || null);
      const updated = note ? notes.map((item) => (item.id === note.id ? next : item)) : [...notes, next];
      void this.saveNotes(updated).then(done);
    });
    cancel.addEventListener("click", done);
    const editor = h("div", { class: "knows-note-editor" }, text, h("div", { class: "knows-note-editor-row" }, scope, spacer(), cancel, save));
    return editor;
  }

  private async saveNotes(notes: ProfileNote[]): Promise<void> {
    try {
      this.profile = await api.profileUpdateNotes(this.target.connectionId, this.target.database, notes);
      this.host.changed(this.target);
    } catch (error) {
      showError(error, "Save the notes");
    }
  }

  // --- Objects -------------------------------------------------------------------------------------------------------------

  private objectsSection(): HTMLElement {
    return section("Objects", h("div", { class: "knows-search-row" }, this.search, this.objectCount), this.objectList);
  }

  private renderObjects(): void {
    const profile = this.profile;
    if (!profile) return;
    const matches = searchObjects(profile, this.search.value);
    this.objectCount.textContent = matches.length === profile.objects.length ? `${matches.length} objects` : `${matches.length} of ${profile.objects.length}`;
    const rows = matches.slice(0, this.shown).map((object) => this.objectRow(profile, object));
    if (matches.length > this.shown) {
      const more = h("button", { class: "btn small knows-more", type: "button" }, `Show ${Math.min(PAGE, matches.length - this.shown)} more`);
      more.addEventListener("click", () => {
        this.shown += PAGE;
        this.renderObjects();
      });
      rows.push(more);
    }
    if (!rows.length) rows.push(h("p", { class: "knows-muted" }, "No object matches."));
    this.objectList.replaceChildren(...rows);
  }

  private objectRow(profile: DatabaseProfile, object: ObjectProfile): HTMLElement {
    const name = qualifiedName(object);
    const row = h("div", { class: "knows-object" });
    const toggle = h(
      "button",
      { class: "knows-object-head", type: "button", "aria-expanded": "false", title: "Columns, value hints and relations" },
      icon("chevron", "knows-chevron"),
      h("span", { class: "knows-object-name" }, name),
      h("span", { class: "knows-object-kind" }, object.kind),
      object.rowEstimate !== undefined ? h("span", { class: "knows-muted knows-small" }, `~${object.rowEstimate.toLocaleString()} rows`) : null,
    );
    toggle.addEventListener("click", () => {
      const open = !row.classList.contains("open");
      row.classList.toggle("open", open);
      toggle.setAttribute("aria-expanded", String(open));
      if (open && !row.querySelector(".knows-object-body")) row.append(objectBody(profile, object));
    });
    const signature = object.detailed
      ? h("div", { class: "knows-signature", title: columnSignature(object.columns, 1000) }, columnSignature(object.columns))
      : h("div", { class: "knows-muted knows-small knows-signature" }, "Name only (past the detail limit)");
    row.append(toggle, signature, this.descriptionLine(object));
    return row;
  }

  private descriptionLine(object: ObjectProfile): HTMLElement {
    const host = h("div", { class: "knows-description" });
    const show = () => {
      const description = object.description;
      const edit = toolButton(
        "edit",
        description ? "Edit" : "Describe",
        () => focusEditor(host, editor(), "fill"),
        "Write your own description (the model never replaces it)",
      );
      host.replaceChildren(
        description ? h("span", { class: "knows-description-text" }, description.text) : h("span", { class: "knows-muted" }, "No description."),
        ...(description?.source === "user" ? [h("span", { class: "knows-tag" }, "edited by you")] : []),
        edit,
      );
    };
    const editor = () => {
      const input = h("input", {
        class: "input knows-description-input",
        type: "text",
        maxlength: "200",
        spellcheck: "true",
        placeholder: "What this object is for",
      });
      input.value = object.description?.text ?? "";
      const save = async (text: string | null) => {
        try {
          const updated = await api.profileUpdateDescription(this.target.connectionId, this.target.database, qualifiedName(object), text);
          this.profile = updated;
          const fresh = updated.objects.find((item) => qualifiedName(item) === qualifiedName(object));
          object.description = fresh?.description;
          this.host.changed(this.target);
          show();
        } catch (error) {
          showError(error, "Save the description");
        }
      };
      input.addEventListener("keydown", (event) => {
        if (event.key === "Enter") void save(input.value.trim() || null);
        if (event.key === "Escape") {
          event.stopPropagation();
          show();
        }
      });
      const ok = h("button", { class: "btn small primary", type: "button" }, "Save");
      ok.addEventListener("click", () => void save(input.value.trim() || null));
      const cancel = h("button", { class: "btn small", type: "button" }, "Cancel");
      cancel.addEventListener("click", show);
      return h("div", { class: "knows-description-editor" }, input, cancel, ok);
    };
    show();
    return host;
  }
}

/** Puts an editor in place (instead of `at`, into it, or after its children) and focuses its text box. */
function focusEditor(at: HTMLElement, editor: HTMLElement, how: "replace" | "fill" | "append"): void {
  if (how === "replace") at.replaceWith(editor);
  else if (how === "fill") at.replaceChildren(editor);
  else at.append(editor);
  editor.querySelector<HTMLElement>("textarea, input")?.focus();
}

/** The badge shown when the schema changed since learning; clicking it refreshes. */
export function staleBadge(refresh: () => void): HTMLElement {
  const badge = h(
    "button",
    { class: "knows-stale", type: "button", title: "Tables or columns changed since the assistant learned this database" },
    icon("refresh"),
    "Schema changed — refresh",
  );
  badge.addEventListener("click", refresh);
  return badge;
}

function section(title: string, ...children: (HTMLElement | null)[]): HTMLElement {
  return h("section", { class: "knows-section" }, h("h3", { class: "knows-section-title" }, title), ...children);
}

/** Columns with their value hints, keys, indexes and relations of one object. */
function objectBody(profile: DatabaseProfile, object: ObjectProfile): HTMLElement {
  const hints = new Map((object.valueHints ?? []).map((hint) => [hint.column, hintText(hint)]));
  const columns = h(
    "table",
    { class: "knows-columns" },
    h("thead", {}, h("tr", {}, h("th", {}, "Column"), h("th", {}, "Type"), h("th", {}, "Values seen"))),
    h(
      "tbody",
      {},
      ...object.columns.map((column) =>
        h(
          "tr",
          {},
          h(
            "td",
            { class: "mono" },
            column.name,
            column.primary ? h("span", { class: "knows-tag" }, "PK") : null,
            column.sensitive ? h("span", { class: "knows-tag muted", title: "Looks like a secret or personal data: never sampled" }, "sensitive") : null,
          ),
          h(
            "td",
            { class: "mono" },
            `${column.typeName}${column.nullable ? "" : " not null"}${column.presence !== undefined ? ` · in ${Math.round(column.presence)}%` : ""}`,
          ),
          h("td", { class: "knows-hint" }, hints.get(column.name) ?? (column.comment ? column.comment : "")),
        ),
      ),
    ),
  );
  const relations = relationsOf(profile, object).map((relation) =>
    h(
      "li",
      {},
      h(
        "span",
        { class: "mono" },
        `${relation.from}(${relation.columns.join(", ")}) → ${relation.toDatabase ? `${relation.toDatabase}.` : ""}${relation.to}(${relation.toColumns.join(", ")})`,
      ),
      h(
        "span",
        { class: "knows-tag muted" },
        relation.source === "fk"
          ? "foreign key"
          : relation.source === "mongoRef"
            ? `reference ${Math.round(relation.confidence * 100)}%`
            : `by name ${Math.round(relation.confidence * 100)}%`,
      ),
    ),
  );
  const indexes = (object.indexes ?? []).map((index) =>
    h("li", { class: "mono" }, `${index.name} (${index.columns.join(", ")})${index.unique ? " unique" : ""}${index.method ? ` ${index.method}` : ""}`),
  );
  return h(
    "div",
    { class: "knows-object-body" },
    object.comment ? h("p", { class: "knows-muted" }, `Comment: ${object.comment}`) : null,
    object.columns.length ? columns : null,
    relations.length ? h("div", {}, h("div", { class: "knows-subtitle" }, "Relations"), h("ul", { class: "knows-list" }, ...relations)) : null,
    indexes.length ? h("div", {}, h("div", { class: "knows-subtitle" }, "Indexes"), h("ul", { class: "knows-list" }, ...indexes)) : null,
  );
}
