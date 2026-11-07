import { api } from "../../core/api";
import { field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import { driverInfo } from "../../core/drivers";
import type { ConnectionProfile } from "../../core/types";
import { withConfirmation } from "../safety/confirmationDialog";

/** "New database…" on a connection. Resolves with the created name, or null when cancelled. */
export function databaseDialog(profile: ConnectionProfile): Promise<string | null> {
  return new Promise((resolve) => {
    let created: string | null = null;
    const info = driverInfo(profile.driver);
    const mongo = info.id === "mongodb";
    const name = textInput("", { class: "input mono", placeholder: mongo ? "e.g. analytics" : "e.g. shop_staging" });
    const collection = textInput("", { class: "input mono", placeholder: "e.g. events" });
    const note =
      info.id === "mssql"
        ? "Created with the server's default size and collation."
        : mongo
          ? "MongoDB only keeps a database once it holds a collection, so the first one is created with it."
          : "Created with utf8mb4 / utf8mb4_unicode_ci.";
    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "On ", h("strong", {}, profile.name), ` (${info.label}). ${note}`),
      field("Database name", name),
      mongo ? field("First collection", collection) : null,
    );
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
            const done = await withConfirmation((confirmation) => api.createDatabase(profile.id, value, mongo ? collection.value.trim() : null, confirmation));
            if (done === null) return false;
            created = value;
          },
        },
      ],
      { width: 440, onClose: () => resolve(created) },
    );
  });
}
