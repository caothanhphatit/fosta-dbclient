import { describe, expect, it } from "vitest";
import type { SchemaGraph, SchemaTable } from "../../core/contract";
import { boxHeightFor, buildLinks, GAP_X, GAP_Y, HEADER, layoutBoxes, ROW } from "./diagramLayout";

const box = (name: string, width: number, height: number) => ({ name, width, height });

const table = (name: string, columns: [string, boolean][]): SchemaTable => ({
  name,
  kind: "table",
  rows: null,
  comment: "",
  columns: columns.map(([column, primary]) => ({ name: column, columnType: "int", nullable: false, primary })),
});

describe("boxHeightFor", () => {
  it("grows by one row per column and keeps a row for an empty box", () => {
    expect(boxHeightFor(3)).toBe(HEADER + 3 * ROW + 6);
    expect(boxHeightFor(0)).toBe(boxHeightFor(1));
  });
});

describe("layoutBoxes", () => {
  it("puts a referenced table one column left of the table that references it, centred on the tallest column", () => {
    const placed = layoutBoxes([box("a", 200, 100), box("b", 150, 80)], [{ from: "a", to: "b" }]);
    expect(placed.get("b")).toEqual({ x: 0, y: 10 });
    expect(placed.get("a")).toEqual({ x: 150 + GAP_X, y: 0 });
  });

  it("gives finite positions to a two-table foreign-key cycle", () => {
    const placed = layoutBoxes(
      [box("a", 100, 50), box("b", 100, 50)],
      [
        { from: "a", to: "b" },
        { from: "b", to: "a" },
      ],
    );
    expect(placed.size).toBe(2);
    for (const position of placed.values()) {
      expect(Number.isFinite(position.x)).toBe(true);
      expect(Number.isFinite(position.y)).toBe(true);
    }
    expect(placed.get("a")).not.toEqual(placed.get("b"));
  });

  it("ignores self references", () => {
    const placed = layoutBoxes([box("a", 100, 50), box("b", 100, 50)], [{ from: "a", to: "a" }]);
    expect(placed.get("a")).toEqual({ x: 0, y: 0 });
    expect(placed.get("b")).toEqual({ x: 100 + GAP_Y, y: 0 });
  });

  it("packs unlinked tables in name order, wrapping at the grid width", () => {
    const wide = [box("e", 600, 50), box("c", 600, 50), box("d", 600, 50)];
    const placed = layoutBoxes(wide, []);
    expect(placed.get("c")).toEqual({ x: 0, y: 0 });
    expect(placed.get("d")).toEqual({ x: 0, y: 50 + GAP_Y });
    expect(placed.get("e")).toEqual({ x: 0, y: 2 * (50 + GAP_Y) });
    const narrow = layoutBoxes([box("x", 100, 50), box("y", 100, 50), box("z", 100, 50)], []);
    expect([...narrow.values()]).toEqual([
      { x: 0, y: 0 },
      { x: 100 + GAP_Y, y: 0 },
      { x: 2 * (100 + GAP_Y), y: 0 },
    ]);
  });

  it("places unlinked tables below the linked ones", () => {
    const placed = layoutBoxes([box("a", 200, 100), box("b", 150, 80), box("z", 100, 40)], [{ from: "a", to: "b" }]);
    // The tallest column ends at 100 + GAP_Y; the grid starts two gaps below that.
    expect(placed.get("z")).toEqual({ x: 0, y: 100 + GAP_Y + 2 * GAP_Y });
  });

  it("orders the boxes of a column by the position of their neighbours", () => {
    // p1 and p2 are referenced by c1 and c2 respectively; c2 is listed first but p2 sorts after p1.
    const boxes = [box("c2", 100, 50), box("c1", 100, 50), box("p1", 100, 50), box("p2", 100, 50)];
    const links = [
      { from: "c1", to: "p1" },
      { from: "c2", to: "p2" },
    ];
    const placed = layoutBoxes(boxes, links);
    expect(placed.get("c1")!.y).toBeLessThan(placed.get("c2")!.y);
    expect(placed.get("p1")!.y).toBeLessThan(placed.get("p2")!.y);
  });
});

describe("buildLinks", () => {
  const graph = (relations: SchemaGraph["relations"], tables: SchemaTable[]): SchemaGraph => ({ tables, relations, totalTables: tables.length });

  it("keeps declared foreign keys and adds none when inference is off", () => {
    const tables = [
      table("users", [["id", true]]),
      table("orders", [
        ["id", true],
        ["user_id", false],
      ]),
    ];
    const relations = [
      { constraint: "fk", database: "d", table: "orders", columns: ["user_id"], referencedDatabase: "d", referencedTable: "users", referencedColumns: ["id"] },
    ];
    const links = buildLinks(graph(relations, tables), false);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({ name: "fk", from: "orders", to: "users", inferred: false });
  });

  it("infers links from field names such as userId and category_id", () => {
    const tables = [
      table("users", [["_id", true]]),
      table("categories", [["id", true]]),
      table("posts", [
        ["id", true],
        ["userId", false],
        ["category_id", false],
        ["tagId", false],
      ]),
    ];
    const links = buildLinks(graph([], tables), true);
    expect(links.map((link) => [link.from, link.fromColumns[0], link.to, link.toColumns[0], link.inferred])).toEqual([
      ["posts", "userId", "users", "_id", true],
      ["posts", "category_id", "categories", "id", true],
    ]);
  });

  it("does not infer over a declared key or from a primary key", () => {
    const tables = [table("users", [["id", true]]), table("orders", [["user_id", true]]), table("notes", [["user_id", false]])];
    const relations = [
      { constraint: "fk", database: "d", table: "notes", columns: ["user_id"], referencedDatabase: "d", referencedTable: "users", referencedColumns: ["id"] },
    ];
    expect(buildLinks(graph(relations, tables), true)).toHaveLength(1);
  });
});
