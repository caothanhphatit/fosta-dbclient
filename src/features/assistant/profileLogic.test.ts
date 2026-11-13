import { describe, expect, it } from "vitest";
import type { DatabaseProfile, ObjectProfile, RelationProfile } from "../../core/profileTypes";
import type { AppConfig, Workspace } from "../../core/types";
import { presetSettings } from "../safety/safety";
import {
  describedText,
  columnSignature,
  editedNote,
  formatTokens,
  isStopped,
  LEARN_STEPS,
  learningAllowed,
  learningRefusal,
  progressIndex,
  readsAllowed,
  relationTag,
  searchObjects,
  sizeText,
  summaryCount,
} from "./profileLogic";

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

describe("the learn dialog", () => {
  const order = LEARN_STEPS.map((item) => item.step);

  it("never moves the progress list back to an earlier step", () => {
    const relations = order.indexOf("relations");
    expect(progressIndex(order, -1, "listing")).toBe(0);
    expect(progressIndex(order, relations, "relations")).toBe(relations);
    // MongoDB's document sampling comes after the relations, as its own step.
    expect(order.indexOf("sampling")).toBe(relations + 1);
    expect(progressIndex(order, relations, "sampling")).toBe(relations + 1);
    expect(progressIndex(order, relations, "describing")).toBeNull();
    expect(progressIndex(order, 0, "summary")).toBeNull();
  });

  it("tells a stop from a failure by the error's code", () => {
    expect(isStopped({ code: "CANCELLED", message: "The request was cancelled" })).toBe(true);
    expect(isStopped("Stopped")).toBe(true);
    expect(isStopped({ code: "ENGINE", message: "connection refused" })).toBe(false);
    expect(isStopped("The request was cancelled by the server")).toBe(false);
  });

  it("counts the summary step in objects, like the cost", () => {
    // 12 objects to describe: the run reports 13 requests' worth (the last one is the database summary).
    expect(summaryCount(0, 13)).toBe("0 / 12 objects");
    expect(summaryCount(10, 13)).toBe("10 / 12 objects");
    expect(summaryCount(12, 13)).toBe("writing the database summary");
    expect(summaryCount(13, 13)).toBe("");
    expect(summaryCount(0, 1)).toBe("writing the database summary");
  });

  it("reports Described like the cost after a run, with the total when earlier descriptions add to it", () => {
    // The cost said 11 objects to describe; one object was described by an earlier run.
    expect(describedText(11, 12)).toBe("11 (12 in all)");
    expect(describedText(11, 11)).toBe("11");
    expect(describedText(11, null)).toBe("11");
    // Stopped or failed before any summary: what the profile holds.
    expect(describedText(null, 12)).toBe("12");
    expect(describedText(null, null)).toBe("0");
  });
});

describe("what the assistant knows", () => {
  const relation = (rest: Partial<RelationProfile>): RelationProfile => ({
    from: "order_items",
    columns: ["order_id"],
    to: "orders",
    toColumns: ["id"],
    source: "fk",
    confidence: 1,
    ...rest,
  });

  it("tags declared keys with the actions that do something", () => {
    expect(relationTag(relation({}))).toBe("foreign key");
    expect(relationTag(relation({ onDelete: "CASCADE", onUpdate: "NO ACTION" }))).toBe("foreign key · on delete cascade");
    expect(relationTag(relation({ onDelete: "RESTRICT", onUpdate: "SET NULL" }))).toBe("foreign key · on update set null");
    expect(relationTag(relation({ source: "inferredName", confidence: 0.8 }))).toBe("by name 80%");
    expect(relationTag(relation({ source: "mongoRef", confidence: 1 }))).toBe("reference 100%");
  });

  it("counts documents in MongoDB collections and rows in tables", () => {
    const object = (kind: string, rowEstimate?: number) => ({ name: "orders", kind, columns: [], rowEstimate }) as unknown as ObjectProfile;
    expect(sizeText(object("collection", 10))).toBe("~10 documents");
    expect(sizeText(object("collection", 1))).toBe("~1 document");
    expect(sizeText(object("table", 1200))).toBe(`~${(1200).toLocaleString()} rows`);
    expect(sizeText(object("table"))).toBeNull();
  });
});
