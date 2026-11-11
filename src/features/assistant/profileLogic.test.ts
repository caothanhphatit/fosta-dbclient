import { describe, expect, it } from "vitest";
import type { DatabaseProfile, ObjectProfile } from "../../core/profileTypes";
import type { AppConfig, Workspace } from "../../core/types";
import { presetSettings } from "../safety/safety";
import { columnSignature, editedNote, formatTokens, learningAllowed, learningRefusal, readsAllowed, searchObjects } from "./profileLogic";

function config(workspace: Partial<Workspace>, rest: Partial<AppConfig> = {}): AppConfig {
  return {
    workspaces: [
      {
        id: "w",
        name: "Team",
        color: "blue",
        icon: "laptop",
        safety: presetSettings("open"),
        connections: [{ id: "c", name: "Main", driver: "postgres" } as Workspace["connections"][number]],
        tasks: [],
        ...workspace,
      },
    ],
    activeWorkspace: "w",
    embedding: null,
    aiConnections: [],
    aiActive: null,
    queryHistory: true,
    ...rest,
  };
}

describe("the settings", () => {
  it("allow learning unless turned off, the workspace override winning", () => {
    expect(learningAllowed(config({}), "c")).toBe(true);
    expect(learningAllowed(config({}, { assistantLearn: false }), "c")).toBe(false);
    expect(learningAllowed(config({ assistantLearn: "allow" }, { assistantLearn: false }), "c")).toBe(true);
    expect(learningAllowed(config({ assistantLearn: "deny" }), "c")).toBe(false);
    expect(learningRefusal(config({ assistantLearn: "deny" }), "c")).toContain("Team workspace");
    expect(learningRefusal(config({}, { assistantLearn: false }), "c")).toContain("Settings → Assistant may learn databases");
  });

  it("allow sampling values only where the assistant may read data", () => {
    expect(readsAllowed(config({}), "c")).toBe(true);
    expect(readsAllowed(config({}, { assistantReadQueries: false }), "c")).toBe(false);
    expect(readsAllowed(config({ safety: presetSettings("readOnly") }), "c")).toBe(false);
    expect(readsAllowed(config({ icon: "shield" }), "c")).toBe(false);
    expect(readsAllowed(config({ icon: "shield", assistantQueries: "allow" }), "c")).toBe(true);
    expect(readsAllowed(config({ assistantQueries: "deny" }), "c")).toBe(false);
    expect(readsAllowed(config({}), "unknown")).toBe(false);
  });
});

const orders: ObjectProfile = {
  name: "orders",
  schema: "public",
  kind: "table",
  fingerprint: "f",
  detailed: true,
  columns: [
    { name: "id", typeName: "int", kind: { kind: "int" }, nullable: false, primary: true },
    { name: "status", typeName: "text", kind: { kind: "text" }, nullable: true },
  ],
  description: { text: "Customer purchases", source: "llm", updatedAt: 0 },
};

const profile = { objects: [orders, { ...orders, name: "items", description: undefined }] } as unknown as DatabaseProfile;

describe("the knowledge view", () => {
  it("writes signatures, token counts and notes", () => {
    expect(columnSignature(orders.columns)).toBe("id int PK, status text?");
    expect(columnSignature(orders.columns, 1)).toBe("id int PK, +1 more");
    expect([formatTokens(980), formatTokens(1234), formatTokens(34_567)]).toEqual(["980", "1.2k", "35k"]);
    const note = editedNote(null, "Status 3 is refunded", "public.orders", 5);
    expect(note).toMatchObject({ text: "Status 3 is refunded", object: "public.orders", createdAt: 5, updatedAt: 5 });
    expect(editedNote(note, "Changed", null, 9)).toMatchObject({ id: note.id, object: null, createdAt: 5, updatedAt: 9 });
  });

  it("searches names, columns and descriptions, every word", () => {
    expect(searchObjects(profile, "").length).toBe(2);
    expect(searchObjects(profile, "purchases").map((object) => object.name)).toEqual(["orders"]);
    expect(searchObjects(profile, "public status").length).toBe(2);
    expect(searchObjects(profile, "items status").map((object) => object.name)).toEqual(["items"]);
    expect(searchObjects(profile, "nothing")).toEqual([]);
  });
});
