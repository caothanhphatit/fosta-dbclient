/**
 * The pure text logic of the assistant panel (no DOM): the markdown subset the chat renders, which conversation a
 * context belongs to, and how tool calls and their cells read in the chat. `assistantPanel.ts` turns the blocks into
 * elements with text nodes only (nothing the model writes becomes HTML).
 */

export type Align = "left" | "center" | "right" | null;

export type Block =
  | { type: "paragraph"; text: string }
  | { type: "heading"; text: string }
  | { type: "list"; ordered: boolean; items: string[] }
  | { type: "code"; language: string; code: string }
  | { type: "table"; header: string[]; align: Align[]; rows: string[][] };

const CODE_FENCE = /^```([\w+-]*)\s*$/;
const TABLE_SEPARATOR_CELL = /^:?-{1,}:?$/;

/** The cells of a GFM table row: outer pipes dropped, `\|` kept as a literal pipe, cells trimmed. */
export function tableCells(line: string): string[] {
  let text = line.trim();
  if (text.startsWith("|")) text = text.slice(1);
  if (text.endsWith("|") && !text.endsWith("\\|")) text = text.slice(0, -1);
  const cells: string[] = [];
  let cell = "";
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (char === "\\" && text[index + 1] === "|") {
      cell += "|";
      index += 1;
    } else if (char === "|") {
      cells.push(cell.trim());
      cell = "";
    } else cell += char;
  }
  cells.push(cell.trim());
  return cells;
}

/** The column alignments of a GFM delimiter row (`|---|:--:|--:|`), or null when `line` is not one. */
export function tableAlignments(line: string): Align[] | null {
  if (!line.includes("-") || !line.includes("|")) return null;
  const cells = tableCells(line);
  if (!cells.every((cell) => TABLE_SEPARATOR_CELL.test(cell))) return null;
  return cells.map((cell) => {
    const left = cell.startsWith(":");
    const right = cell.endsWith(":");
    return left && right ? "center" : right ? "right" : left ? "left" : null;
  });
}

function isTableRow(line: string): boolean {
  return line.trim() !== "" && line.includes("|");
}

/** The markdown subset of the chat: fenced code, headings, lists, GFM tables and paragraphs. */
export function parseMarkdown(text: string): Block[] {
  const blocks: Block[] = [];
  const lines = text.split("\n");
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  const flushParagraph = () => {
    if (paragraph.length) blocks.push({ type: "paragraph", text: paragraph.join(" ") });
    paragraph = [];
  };
  const flushList = () => {
    if (list) blocks.push({ type: "list", ordered: list.ordered, items: list.items });
    list = null;
  };
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const fenceMatch = CODE_FENCE.exec(line.trim());
    if (fenceMatch) {
      flushParagraph();
      flushList();
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !lines[index].trim().startsWith("```")) {
        code.push(lines[index]);
        index += 1;
      }
      blocks.push({ type: "code", language: fenceMatch[1].toLowerCase(), code: code.join("\n") });
      continue;
    }
    // A table: a row with pipes followed by a delimiter row; rows follow until a line without a pipe.
    const align = isTableRow(line) && index + 1 < lines.length ? tableAlignments(lines[index + 1]) : null;
    if (align) {
      const header = tableCells(line);
      if (header.length === align.length) {
        flushParagraph();
        flushList();
        index += 2;
        const rows: string[][] = [];
        while (index < lines.length && isTableRow(lines[index]) && !CODE_FENCE.test(lines[index].trim())) {
          const cells = tableCells(lines[index]);
          rows.push(header.map((_, column) => cells[column] ?? ""));
          index += 1;
        }
        index -= 1;
        blocks.push({ type: "table", header, align, rows });
        continue;
      }
    }
    const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (bullet || numbered) {
      flushParagraph();
      const ordered = Boolean(numbered);
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push((bullet ?? numbered)![1]);
      continue;
    }
    if (!line.trim()) {
      flushParagraph();
      flushList();
      continue;
    }
    const heading = /^#{1,4}\s+(.*)$/.exec(line);
    if (heading) {
      flushParagraph();
      flushList();
      blocks.push({ type: "heading", text: heading[1] });
      continue;
    }
    if (list) list.items[list.items.length - 1] += ` ${line.trim()}`;
    else paragraph.push(line.trim());
  }
  flushParagraph();
  flushList();
  return blocks;
}

// --- Conversations -----------------------------------------------------------------------------------------------------

/** What a conversation is about: a connection and one of its databases ("" when the context names none). */
export interface ChatTopic {
  connectionId: string;
  database: string;
}

export function topicKey(topic: ChatTopic): string {
  return `${topic.connectionId}|${topic.database}`;
}

/**
 * Which conversation a context belongs to. `stay` keeps the open one: a context without a connection (nothing to mix
 * with), or the same connection where the context names no other database (a tab that knows only the connection).
 * `adopt` ties a conversation that has no topic yet to this one; `switch` opens the topic's own conversation (the one
 * kept from before, or a new one), so another connection's or database's history is never sent along.
 */
export function conversationMove(
  current: ChatTopic | null,
  context: { connectionId?: string; database?: string },
): { kind: "stay" } | { kind: "adopt" | "switch"; topic: ChatTopic } {
  if (!context.connectionId) return { kind: "stay" };
  const topic = { connectionId: context.connectionId, database: context.database ?? "" };
  if (!current) return { kind: "adopt", topic };
  if (current.connectionId !== topic.connectionId) return { kind: "switch", topic };
  if (!topic.database || topic.database === current.database) return { kind: "stay" };
  return { kind: "switch", topic };
}

// --- Tool calls ---------------------------------------------------------------------------------------------------------

const MONGO_STATEMENT = /^db\.getCollection\(("(?:[^"\\]|\\.)*")\)\.(find|aggregate)\(/;

/** The collection a MongoDB tool call read (its statement `db.getCollection("orders").find(…)`), or null. */
function mongoCall(statement: string): { collection: string; operation: string } | null {
  const match = MONGO_STATEMENT.exec(statement);
  if (!match) return null;
  try {
    return { collection: JSON.parse(match[1]) as string, operation: match[2] };
  } catch {
    return null;
  }
}

/** The title of a read the assistant ran: documents it sampled, a pipeline it previewed, or a read-only query. */
export function toolCallTitle(statement: string): string {
  const mongo = mongoCall(statement);
  if (!mongo) return "Ran a read-only query";
  return mongo.operation === "aggregate" ? `Previewed a pipeline on ${mongo.collection}` : `Sampled documents from ${mongo.collection}`;
}

/** Whether the cells of a tool call hold MongoDB Extended JSON (shown in shell syntax). */
export function isDocumentCall(statement: string): boolean {
  return mongoCall(statement) !== null;
}

const EJSON_WRAPPERS: [RegExp, (value: string) => string][] = [
  [/\{\s*"\$oid"\s*:\s*"([0-9a-fA-F]{24})"\s*\}/g, (value) => `ObjectId("${value}")`],
  [/\{\s*"\$date"\s*:\s*"([^"\\]*)"\s*\}/g, (value) => `ISODate("${value}")`],
  [/\{\s*"\$date"\s*:\s*\{\s*"\$numberLong"\s*:\s*"(-?\d+)"\s*\}\s*\}/g, (value) => `ISODate("${millisText(value)}")`],
  [/\{\s*"\$numberDecimal"\s*:\s*"([^"\\]*)"\s*\}/g, (value) => `NumberDecimal("${value}")`],
  [/\{\s*"\$numberLong"\s*:\s*"(-?\d+)"\s*\}/g, (value) => `NumberLong("${value}")`],
  [/\{\s*"\$uuid"\s*:\s*"([0-9a-fA-F-]{36})"\s*\}/g, (value) => `UUID("${value}")`],
];

function millisText(millis: string): string {
  const date = new Date(Number(millis));
  return Number.isNaN(date.getTime()) ? millis : date.toISOString();
}

/**
 * A cell of sampled documents as the rest of the app shows values (shell syntax): `{"$oid":"…"}` reads
 * `ObjectId("…")`, dates `ISODate("…")`, and so on. Works on cut cells too (a rewrite of whole wrappers only).
 */
export function shellCell(text: string): string {
  if (!text.includes("{")) return text;
  let shown = text;
  for (const [pattern, replace] of EJSON_WRAPPERS) shown = shown.replace(pattern, (_, value: string) => replace(value));
  return shown;
}

// --- Quick actions -------------------------------------------------------------------------------------------------------

/** How the quick actions name an object and its records: a collection holds documents, a table rows. */
export function objectWords(kind: string | undefined): { object: string; records: string; describe: string } {
  if (kind === "collection") {
    return {
      object: "collection",
      records: "documents",
      describe: "what each field is for, its indexes, and which collections it references",
    };
  }
  return { object: "table", records: "rows", describe: "what each column is for, the keys, and how it relates to other tables" };
}

/**
 * Whether the "Thinking…" row shows after a chat event: from a tool call (it runs, then the model works on its result)
 * until the model writes again; null leaves it as it is.
 */
export function thinkingAfter(event: { type: string }): boolean | null {
  switch (event.type) {
    case "tool":
    case "query":
      return true;
    case "text":
    case "done":
      return false;
    default:
      return null;
  }
}

/**
 * Whether the answer bubble hides while the "Thinking…" row shows: only while nothing is written yet, so an empty bubble
 * with a lone cursor never sits under the row.
 */
export function hideAnswerWhileThinking(thinking: boolean, reply: string): boolean {
  return thinking && reply.trim() === "";
}

/**
 * The context row's text in two parts: the connection, which gives way first (ellipsis) when the row is narrow, and
 * the place in it (database · table), which stays readable.
 */
export function contextParts(
  connectionName: string | undefined,
  database: string | undefined,
  table: string | undefined,
): { connection: string; place: string } {
  return {
    connection: connectionName || "connection",
    place: [database, table].filter(Boolean).join(" · "),
  };
}

/**
 * Whether a statement that may have changed the schema on `connectionId` makes the panel check what the assistant knows
 * again: only for the chat's own connection (`knowledgeKey` is "connection|database").
 */
export function recheckAfterSchemaChange(knowledgeKey: string, connectionId: string): boolean {
  return connectionId !== "" && knowledgeKey.startsWith(`${connectionId}|`);
}
