import { describe, expect, it } from "vitest";
import {
  contextParts,
  conversationMove,
  hideAnswerWhileThinking,
  isDocumentCall,
  objectWords,
  parseMarkdown,
  recheckAfterSchemaChange,
  shellCell,
  tableAlignments,
  tableCells,
  thinkingAfter,
  toolCallTitle,
} from "./chatText";

describe("markdown tables", () => {
  it("render a GFM table as a table block, not one line of pipes", () => {
    const text = "VIP customers per country:\n\n| Country | VIP customers |\n|---|---:|\n| DE | 24 |\n| FR | 17 |\n\nTotal: 41.";
    expect(parseMarkdown(text)).toEqual([
      { type: "paragraph", text: "VIP customers per country:" },
      {
        type: "table",
        header: ["Country", "VIP customers"],
        align: [null, "right"],
        rows: [
          ["DE", "24"],
          ["FR", "17"],
        ],
      },
      { type: "paragraph", text: "Total: 41." },
    ]);
  });

  it("end at the first line without a pipe, and accept rows without outer pipes", () => {
    const blocks = parseMarkdown("a | b\n:-- | :-:\n1 | 2\nafter");
    expect(blocks).toEqual([
      { type: "table", header: ["a", "b"], align: ["left", "center"], rows: [["1", "2"]] },
      { type: "paragraph", text: "after" },
    ]);
  });

  it("pad short rows, drop extra cells, and keep escaped pipes as text", () => {
    const [table] = parseMarkdown("| a | b |\n|---|---|\n| x |\n| 1 | 2 | 3 |\n| `a \\| b` | y |");
    expect(table).toEqual({
      type: "table",
      header: ["a", "b"],
      align: [null, null],
      rows: [
        ["x", ""],
        ["1", "2"],
        ["`a | b`", "y"],
      ],
    });
  });

  it("need a delimiter row with as many cells as the header", () => {
    expect(parseMarkdown("| a | b |\n| c | d |")).toEqual([{ type: "paragraph", text: "| a | b | | c | d |" }]);
    expect(parseMarkdown("| a | b |\n|---|")[0].type).toBe("paragraph");
  });

  it("parse cells and delimiter rows", () => {
    expect(tableCells("| a | `b` |")).toEqual(["a", "`b`"]);
    expect(tableAlignments("|---|:---:|--:|:--|")).toEqual([null, "center", "right", "left"]);
    expect(tableAlignments("| a | b |")).toBeNull();
    expect(tableAlignments("--- not a row")).toBeNull();
  });

  it("leave code blocks, lists and headings as before", () => {
    expect(parseMarkdown("## Title\n- one\n- two\n```sql\nselect 1 | 2\n```")).toEqual([
      { type: "heading", text: "Title" },
      { type: "list", ordered: false, items: ["one", "two"] },
      { type: "code", language: "sql", code: "select 1 | 2" },
    ]);
  });
});

describe("conversations", () => {
  const shop = { connectionId: "pg", database: "shop" };

  it("tie a new chat to the first connection it is about", () => {
    expect(conversationMove(null, { connectionId: "pg", database: "shop" })).toEqual({ kind: "adopt", topic: shop });
    expect(conversationMove(null, {})).toEqual({ kind: "stay" });
  });

  it("switch when the connection or the database changes", () => {
    expect(conversationMove(shop, { connectionId: "mongo", database: "fosta_extras" })).toEqual({
      kind: "switch",
      topic: { connectionId: "mongo", database: "fosta_extras" },
    });
    expect(conversationMove(shop, { connectionId: "pg", database: "billing" })).toEqual({ kind: "switch", topic: { connectionId: "pg", database: "billing" } });
    expect(conversationMove(shop, { connectionId: "mongo" })).toEqual({ kind: "switch", topic: { connectionId: "mongo", database: "" } });
  });

  it("stay on the same database, a context naming no database on the same connection, or no connection at all", () => {
    expect(conversationMove(shop, { connectionId: "pg", database: "shop" })).toEqual({ kind: "stay" });
    expect(conversationMove(shop, { connectionId: "pg" })).toEqual({ kind: "stay" });
    expect(conversationMove(shop, {})).toEqual({ kind: "stay" });
  });
});

describe("tool calls", () => {
  it("label sampled documents and pipeline previews apart from read-only queries", () => {
    expect(toolCallTitle('db.getCollection("orders").find({"status":"paid"}).limit(5)')).toBe("Sampled documents from orders");
    expect(toolCallTitle('db.getCollection("my \\"odd\\" one").find({}).limit(5)')).toBe('Sampled documents from my "odd" one');
    expect(toolCallTitle('db.getCollection("orders").aggregate([{"$match":{}}]) on a sample, documents leaving stage 1')).toBe(
      "Previewed a pipeline on orders",
    );
    expect(toolCallTitle("SELECT status, count(*) FROM orders GROUP BY status")).toBe("Ran a read-only query");
    expect(isDocumentCall('db.getCollection("orders").find({}).limit(5)')).toBe(true);
    expect(isDocumentCall("SELECT 1")).toBe(false);
  });

  it("show Extended JSON values in shell syntax", () => {
    expect(shellCell('{"$oid":"65f1a2b3c4d5e6f708192a3b"}')).toBe('ObjectId("65f1a2b3c4d5e6f708192a3b")');
    expect(shellCell('{"customerId":{"$oid":"65f1a2b3c4d5e6f708192a3b"},"at":{"$date":"2026-01-02T03:04:05Z"}}')).toBe(
      '{"customerId":ObjectId("65f1a2b3c4d5e6f708192a3b"),"at":ISODate("2026-01-02T03:04:05Z")}',
    );
    expect(shellCell('{"$date":{"$numberLong":"0"}}')).toBe('ISODate("1970-01-01T00:00:00.000Z")');
    expect(shellCell('{"$numberDecimal":"9.99"}')).toBe('NumberDecimal("9.99")');
    expect(shellCell('{"$numberLong":"12"}')).toBe('NumberLong("12")');
    // A cut cell keeps what it cannot rewrite.
    expect(shellCell('[{"$oid":"65f1a2b3c4d5e6f708192a3b"},{"$oid":"65f1a2b3c4')).toBe('[ObjectId("65f1a2b3c4d5e6f708192a3b"),{"$oid":"65f1a2b3c4');
    expect(shellCell("plain text")).toBe("plain text");
  });

  it("show a working row from a tool call until the model writes again", () => {
    expect(thinkingAfter({ type: "tool" })).toBe(true);
    expect(thinkingAfter({ type: "query" })).toBe(true);
    expect(thinkingAfter({ type: "text" })).toBe(false);
    expect(thinkingAfter({ type: "done" })).toBe(false);
    expect(thinkingAfter({ type: "action" })).toBeNull();
  });

  it("hide the empty answer (and its cursor) while the Thinking row shows", () => {
    expect(hideAnswerWhileThinking(true, "")).toBe(true);
    expect(hideAnswerWhileThinking(true, " \t ")).toBe(true);
    expect(hideAnswerWhileThinking(true, "Let me check")).toBe(false);
    expect(hideAnswerWhileThinking(false, "")).toBe(false);
  });
});

describe("the context row", () => {
  it("keeps the database apart from the connection, which gives way first", () => {
    expect(contextParts("PG shop", "shop", undefined)).toEqual({ connection: "PG shop", place: "shop" });
    expect(contextParts("Mongo events", "fosta_extras", "orders")).toEqual({ connection: "Mongo events", place: "fosta_extras · orders" });
    expect(contextParts(undefined, undefined, undefined)).toEqual({ connection: "connection", place: "" });
  });

  it("rechecks what the assistant knows after DDL on the chat's connection only", () => {
    expect(recheckAfterSchemaChange("pg1|shop", "pg1")).toBe(true);
    expect(recheckAfterSchemaChange("pg1|", "pg1")).toBe(true);
    expect(recheckAfterSchemaChange("pg10|shop", "pg1")).toBe(false);
    expect(recheckAfterSchemaChange("mongo|events", "pg1")).toBe(false);
    expect(recheckAfterSchemaChange("", "")).toBe(false);
  });
});

describe("quick actions", () => {
  it("speak of documents in a collection and rows in a table", () => {
    expect(objectWords("collection")).toMatchObject({ object: "collection", records: "documents" });
    expect(objectWords("table")).toMatchObject({ object: "table", records: "rows" });
    expect(objectWords(undefined)).toMatchObject({ object: "table", records: "rows" });
  });
});
