import { bindConnection } from "../../core/connectionApi";
import { field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import { engineOf } from "../../core/engines";
import { dialectOf } from "../../core/sql";
import type { ConnectionProfile } from "../../core/types";
import { withConfirmation } from "../safety/confirmationDialog";

/** "New database…" on a connection. Resolves with the created name, or null when cancelled. */
export function databaseDialog(profile: ConnectionProfile): Promise<string | null> {
  return new Promise((resolve) => {
    let created: string | null = null;
    const engine = engineOf(profile);
    // Document stores keep a database only once it holds a collection, so the first one is created with it.
    const document = engine.family === "document";
    const name = textInput("", { class: "input mono", placeholder: document ? "e.g. analytics" : "e.g. shop_staging" });
    const collection = textInput("", { class: "input mono", placeholder: "e.g. events" });
    const note = document
      ? `${engine.label} only keeps a database once it holds a collection, so the first one is created with it.`
      : dialectOf(engine.sqlDialect) === "mysql"
        ? "Created with utf8mb4 / utf8mb4_unicode_ci."
        : "Created with the server's default size and collation.";
    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "On ", h("strong", {}, profile.name), ` (${engine.label}). ${note}`),
      field("Database name", name),
      document ? field("First collection", collection) : null,
    );
    const connection = bindConnection(profile.id);
    openModal(
      "New database",
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Create",
          primary: true,
          run: async () => {
            const value = name.value.trim();
            if (!value) throw new Error("Database name is required");
            // The command resolves with null (no value), which `withConfirmation` also uses for "cancelled".
            const done = await withConfirmation(async (confirmation) => {
              await connection.createDatabase(value, document ? collection.value.trim() : null, confirmation);
              return true;
            });
            if (done === null) return false;
            created = value;
          },
        },
      ],
      { width: 440, onClose: () => resolve(created) },
    );
  });
}
