import { describe, expect, it } from "vitest";
import { cursor } from "../testSchema";
import type { CompletionSchema } from "../types";
import { analyzeMongo, mongoCompletions, mongoStatementAt, mongoStatements } from "./completion";

const SCHEMA: CompletionSchema = {
  databases: ["shop"],
  schemas: [],
  tables: [
    {
      name: "orders",
      kind: "collection",
      columns: [
        { name: "_id", type: "objectId", pk: true },
        { name: "status", type: "string", pk: false },
        { name: "total", type: "double", pk: false },
        { name: "customer.name", type: "string", pk: false },
      ],
    },
    { name: "customers", kind: "collection", columns: [{ name: "email", type: "string", pk: false }] },
    { name: "audit-log", kind: "collection", columns: [] },
  ],
};

const complete = (text: string, explicit = true) => {
  const { doc, pos } = cursor(text);
  return mongoCompletions(SCHEMA, doc, pos, explicit);
};
const labels = (text: string) => complete(text)?.options.map((o) => o.label) ?? [];
const applyOf = (text: string, label: string) => complete(text)?.options.find((o) => o.label === label)?.apply;
const role = (text: string) => {
  const { doc, pos } = cursor(text);
  const ctx = analyzeMongo(doc, pos);
  return ctx.kind === "key" ? ctx.role : ctx.kind;
};

describe("Mongo shell completion", () => {
  it("completes collections and database methods after db.", () => {
    const l = labels("db.|");
    expect(l).toContain("orders");
    expect(l).toContain("getCollection");
    expect(applyOf("db.|", "audit-log")).toBe('getCollection("audit-log")');
  });

  it("completes collection methods and cursor methods", () => {
    expect(labels("db.orders.|")).toContain("aggregate");
    expect(labels('db.getCollection("orders").fi|')).toContain("find");
    expect(labels("db.orders.find({ status: 'paid' }).|")).toContain("sort");
    expect(labels("db.orders.aggregate([]).|")).toContain("toArray");
  });

  it("completes collection names inside getCollection", () => {
    expect(labels('db.getCollection("|")')).toEqual(["orders", "customers", "audit-log"]);
    expect(applyOf('db.getCollection("|', "orders")).toBe('orders"');
  });

  it("knows query, field-operator and update positions", () => {
    expect(role("db.orders.find({ |")).toBe("query");
    expect(labels("db.orders.find({ |")).toEqual(expect.arrayContaining(["status", "total", "$or", "$and"]));
    expect(role("db.orders.find({ total: { $|")).toBe("fieldQuery");
    expect(labels("db.orders.find({ total: { $g|")).toEqual(expect.arrayContaining(["$gt", "$gte"]));
    expect(role("db.orders.find({ $or: [{ |")).toBe("query");
    expect(role("db.orders.updateOne({ _id: 1 }, { |")).toBe("update");
    expect(labels("db.orders.updateOne({ _id: 1 }, { $|")).toContain("$set");
    expect(role("db.orders.updateOne({ _id: 1 }, { $set: { |")).toBe("updateFields");
    expect(role("db.orders.find({}, { |")).toBe("fields");
    expect(role("db.orders.find().sort({ |")).toBe("fields");
  });

  it("knows aggregation stages, accumulators and stage options", () => {
    expect(role("db.orders.aggregate([{ |")).toBe("stage");
    expect(labels("db.orders.aggregate([{ $|")).toEqual(expect.arrayContaining(["$match", "$group", "$lookup", "$unwind"]));
    expect(role("db.orders.aggregate([{ $match: { |")).toBe("query");
    expect(role("db.orders.aggregate([{ $match: {} }, { $group: { |")).toBe("group");
    expect(labels("db.orders.aggregate([{ $group: { |")).toEqual(["_id"]);
    expect(role("db.orders.aggregate([{ $group: { _id: '$status', n: { |")).toBe("accumulator");
    expect(labels("db.orders.aggregate([{ $group: { _id: null, n: { $s|")).toContain("$sum");
    expect(labels("db.orders.aggregate([{ $lookup: { |")).toEqual(["from", "localField", "foreignField", "as", "let", "pipeline"]);
    expect(labels("db.orders.aggregate([{ $lookup: { from: '|")).toContain("customers");
    expect(role("db.orders.aggregate([{ $facet: { a: [{ |")).toBe("stage");
    expect(role("db.orders.aggregate([{ $lookup: { from: 'c', pipeline: [{ |")).toBe("stage");
  });

  it("completes field paths in strings", () => {
    const result = complete("db.orders.aggregate([{ $group: { _id: '$st|");
    expect(result?.options.map((o) => o.label)).toContain("status");
    expect(result?.options.find((o) => o.label === "status")?.apply).toBe("status'");
  });

  it("quotes keys that are not identifiers in shell syntax", () => {
    expect(applyOf("db.orders.find({ c|", "customer.name")).toBe('"customer.name"');
    expect(applyOf("db.orders.find({ s|", "status")).toBe("status");
  });
});

describe("Mongo JSON command completion", () => {
  it("completes command names, collections and options", () => {
    expect(labels('{ "|')).toEqual(expect.arrayContaining(["find", "aggregate", "count"]));
    expect(applyOf('{ "|', "find")).toBe('find"');
    expect(labels('{ "find": "|')).toContain("orders");
    expect(labels('{ "find": "orders", |')).toEqual(expect.arrayContaining(["filter", "projection", "sort", "limit"]));
    expect(applyOf('{ "find": "orders", f|', "filter")).toBe('"filter"');
  });

  it("knows filter and pipeline positions", () => {
    expect(role('{ "find": "orders", "filter": { "|')).toBe("query");
    expect(labels('{ "find": "orders", "filter": { "|')).toContain("status");
    expect(role('{ "aggregate": "orders", "pipeline": [{ "|')).toBe("stage");
    expect(role('{ "update": "orders", "updates": [{ "u": { "|')).toBe("update");
  });
});

describe("Mongo statements", () => {
  it("splits on semicolons and top-level line breaks, keeping chained lines", () => {
    const text = 'db.orders.find({\n  a: 1\n})\n  .sort({ a: 1 })\ndb.customers.count(); db.x.drop()\n\n{ "ping": 1 }';
    const parts = mongoStatements(text).map((r) => text.slice(r.from, r.to));
    expect(parts).toEqual(["db.orders.find({\n  a: 1\n})\n  .sort({ a: 1 })", "db.customers.count()", "db.x.drop()", '{ "ping": 1 }']);
    const r = mongoStatementAt(text, text.indexOf("sort"));
    expect(text.slice(r.from, r.to).startsWith("db.orders.find")).toBe(true);
  });

  it("ignores separators inside strings and comments", () => {
    const text = "db.a.find({ s: 'x;y' }) // ; c\ndb.b.find()";
    expect(mongoStatements(text)).toHaveLength(2);
  });

  it("stays quiet without a trigger unless asked", () => {
    expect(complete("db.orders.find({ |", false)).toBeNull();
    expect(complete("db.orders.find({ s|", false)).not.toBeNull();
    expect(complete("db.orders.find({ s: 'a' // |")).toBeNull();
  });
});
