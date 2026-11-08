/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import type * as C from "./contract";

/**
 * Checks the JSON the Rust contract writes (`__fixtures__/contract/*.json`, from `cargo test contract::fixtures`) against the
 * TypeScript mirrors. Each object shape below is typed with `Keys<T>`, so it must list exactly the fields of the TS type;
 * the runtime check then requires the fixture to have those fields (optional ones may be missing) and nothing else.
 */

type Shape =
  | "string"
  | "number"
  | "boolean"
  | "any"
  | "value"
  | { opt: Shape }
  | { nullable: Shape }
  | { arr: Shape }
  | { tuple: Shape[] }
  | { obj: Record<string, Shape> }
  | { lit: readonly string[] }
  | { union: { tag: string; cases: Record<string, Record<string, Shape>> } }
  | { record: Shape };

type Keys<T> = { [K in keyof Required<T>]: Shape };

const obj = <T>(fields: Keys<T>): Shape => ({ obj: fields as Record<string, Shape> });
const nul = (shape: Shape): Shape => ({ nullable: shape });
const arr = (shape: Shape): Shape => ({ arr: shape });
const opt = (shape: Shape): Shape => ({ opt: shape });
const lit = (...values: string[]): Shape => ({ lit: values });

const VALUE_TAGS = [
  "null",
  "bool",
  "int",
  "float",
  "decimal",
  "text",
  "bytes",
  "date",
  "time",
  "timestamp",
  "timestamptz",
  "interval",
  "uuid",
  "json",
  "objectId",
  "array",
  "doc",
  "other",
];

function checkValue(value: unknown, path: string, errors: string[]): void {
  if (typeof value !== "object" || value === null) return void errors.push(`${path}: not a value`);
  const v = value as Record<string, unknown>;
  const tag = v.t as string;
  if (!VALUE_TAGS.includes(tag)) return void errors.push(`${path}: unknown tag ${String(tag)}`);
  const allowed = tag === "other" ? ["t", "v", "type"] : ["t", "v"];
  for (const key of Object.keys(v)) if (!allowed.includes(key)) errors.push(`${path}: unexpected ${key}`);
  if (tag === "array") (v.v as unknown[]).forEach((item, i) => checkValue(item, `${path}[${i}]`, errors));
  if (tag === "doc") (v.v as [string, unknown][]).forEach(([key, item], i) => checkValue(item, `${path}.${key ?? i}`, errors));
}

function check(value: unknown, shape: Shape, path: string, errors: string[]): void {
  if (shape === "any") return;
  if (shape === "value") return checkValue(value, path, errors);
  if (typeof shape === "string") {
    if (typeof value !== shape) errors.push(`${path}: expected ${shape}, got ${JSON.stringify(value)}`);
    return;
  }
  if ("opt" in shape) return value === undefined ? undefined : check(value, shape.opt, path, errors);
  if ("nullable" in shape) return value === null ? undefined : check(value, shape.nullable, path, errors);
  if ("arr" in shape) {
    if (!Array.isArray(value)) return void errors.push(`${path}: expected an array`);
    return value.forEach((item, i) => check(item, shape.arr, `${path}[${i}]`, errors));
  }
  if ("tuple" in shape) {
    if (!Array.isArray(value) || value.length !== shape.tuple.length) return void errors.push(`${path}: expected a tuple`);
    return shape.tuple.forEach((s, i) => check(value[i], s, `${path}[${i}]`, errors));
  }
  if ("lit" in shape) {
    if (!shape.lit.includes(value as string)) errors.push(`${path}: ${JSON.stringify(value)} is not one of ${shape.lit.join(", ")}`);
    return;
  }
  if ("record" in shape) {
    for (const [key, item] of Object.entries(value as object)) check(item, shape.record, `${path}.${key}`, errors);
    return;
  }
  if (typeof value !== "object" || value === null) return void errors.push(`${path}: expected an object`);
  const record = value as Record<string, unknown>;
  let fields: Record<string, Shape>;
  if ("union" in shape) {
    const tag = record[shape.union.tag] as string;
    const fieldsOf = shape.union.cases[tag];
    if (!fieldsOf) return void errors.push(`${path}: unknown ${shape.union.tag} ${tag}`);
    fields = { [shape.union.tag]: "string", ...fieldsOf };
  } else {
    fields = shape.obj;
  }
  for (const key of Object.keys(record)) if (!(key in fields)) errors.push(`${path}: unexpected field ${key}`);
  for (const [key, fieldShape] of Object.entries(fields)) {
    const optional = typeof fieldShape === "object" && "opt" in fieldShape;
    if (!(key in record) && !optional) errors.push(`${path}: missing field ${key}`);
    else check(record[key], fieldShape, `${path}.${key}`, errors);
  }
}

const valueKind: Shape = { obj: { kind: "string", of: { opt: "any" } } };
const objectRef = obj<C.ObjectRef>({
  database: nul("string"),
  schema: nul("string"),
  name: "string",
  kind: "string",
  path: opt(arr("string")),
  binary: opt("boolean"),
});
const nodeKey = obj<C.NodeKey>({ id: "string", kind: "string", object: nul(objectRef) });
const actionTarget: Shape = {
  union: {
    tag: "kind",
    cases: { view: { role: "string" }, query: { template: "string" }, capability: { name: "string", args: "any" }, builtin: { name: "string" } },
  },
};
const nodeAction = obj<C.NodeAction>({
  id: "string",
  label: "string",
  icon: nul("string"),
  target: actionTarget,
  danger: "boolean",
  separatorBefore: "boolean",
});
const objectNode = obj<C.ObjectNode>({
  key: nodeKey,
  label: "string",
  kind: "string",
  icon: "string",
  object: nul(objectRef),
  detail: nul("string"),
  badges: arr(obj<C.Badge>({ text: "string", tone: lit("neutral", "ok", "warn", "danger") })),
  expandable: "boolean",
  defaultView: nul("string"),
  actions: arr(nodeAction),
});
const columnInfo = obj<C.ColumnInfo>({
  name: "string",
  typeName: "string",
  kind: valueKind,
  nullable: "boolean",
  default: nul("string"),
  comment: nul("string"),
  identity: nul("string"),
  generated: nul("string"),
  hints: obj<C.ColumnHints>({ primary: opt("boolean"), binary: opt("boolean"), vectorDims: opt("number"), readOnly: opt("boolean") }),
});
const keyInfo = obj<C.KeyInfo>({ name: nul("string"), columns: arr("string") });
const field = obj<C.Field>({ column: "string", value: "value" });
const transactionState = obj<C.TransactionState>({ supported: "boolean", open: "boolean", autoCommit: "boolean", failed: "boolean" });
const languageSpec = obj<C.LanguageSpec>({
  id: "string",
  label: "string",
  editor: "string",
  dialect: nul("string"),
  default: "boolean",
  hint: "string",
  fences: arr("string"),
});
const capabilities = arr(
  lit(
    "tree",
    "describe",
    "multipleDatabases",
    "schemas",
    "browseRows",
    "editRows",
    "rawFilter",
    "keysetPaging",
    "estimatedCount",
    "exactCount",
    "query",
    "cancel",
    "transactions",
    "readOnlySession",
    "statementTimeout",
    "streamingResults",
    "schemaGraph",
    "createDatabase",
    "explain",
    "documents",
    "keyValue",
    "search",
    "console",
    "serverOverview",
    "vector",
  ),
);
const filterCond = obj<C.FilterCond>({ column: "string", op: "string", values: arr("string") });
const rowChangeFields = { key: arr(field), original: arr(field), version: opt("value"), force: opt("boolean") };

const SHAPES: Record<string, Shape> = {
  values: arr("value"),
  "tree-request": obj<C.TreeRequest>({ parent: nul(nodeKey), cursor: nul("string"), limit: nul("number"), filter: nul("string") }),
  "tree-children": obj<C.TreeChildren>({ nodes: arr(objectNode), nextCursor: nul("string"), totalHint: nul("number") }),
  "object-description": obj<C.ObjectDescription>({
    object: objectRef,
    comment: nul("string"),
    columns: arr(columnInfo),
    primaryKey: nul(keyInfo),
    uniqueKeys: arr(keyInfo),
    foreignKeys: arr(
      obj<C.ForeignKeyInfo>({
        name: nul("string"),
        columns: arr("string"),
        refDatabase: nul("string"),
        refSchema: nul("string"),
        refTable: "string",
        refColumns: arr("string"),
        onUpdate: nul("string"),
        onDelete: nul("string"),
      }),
    ),
    referencedBy: arr(
      obj<C.ReferencingKey>({
        name: nul("string"),
        database: nul("string"),
        schema: nul("string"),
        table: "string",
        columns: arr("string"),
        refColumns: arr("string"),
      }),
    ),
    indexes: arr(
      obj<C.IndexInfo>({ name: "string", columns: arr("string"), unique: "boolean", primary: "boolean", method: nul("string"), definition: nul("string") }),
    ),
    rowEstimate: nul("number"),
    ddl: nul("string"),
    extra: arr({ tuple: ["string", "string"] }),
  }),
  "browse-request": obj<C.BrowseRequest>({
    object: objectRef,
    filter: {
      union: {
        tag: "kind",
        cases: { none: {}, conditions: { conditions: arr(filterCond), matchAny: "boolean" }, raw: { language: "string", text: "string" } },
      },
    },
    sort: arr(obj<C.SortKey>({ column: "string", descending: "boolean" })),
    offset: "number",
    limit: "number",
    after: nul(arr("value")),
    estimateTotal: "boolean",
    exactTotal: "boolean",
    columns: nul(arr("string")),
  }),
  page: obj<C.Page>({
    columns: arr(columnInfo),
    rows: arr(arr("value")),
    hasMore: "boolean",
    keyColumns: arr("string"),
    nextKeyset: nul(arr("value")),
    total: nul(obj<C.Total>({ value: "number", exact: "boolean" })),
    statement: nul("string"),
    elapsedMs: "number",
    rowVersions: opt(arr("value")),
  }),
  "row-change-set": obj<C.RowChangeSet>({
    object: objectRef,
    changes: arr({
      union: { tag: "op", cases: { insert: { values: arr(field) }, update: { ...rowChangeFields, values: arr(field) }, delete: rowChangeFields } },
    }),
  }),
  "apply-result": obj<C.ApplyResult>({ inserted: "number", updated: "number", deleted: "number" }),
  "conflict-error": obj<C.ConflictError>({
    code: lit("CONFLICT"),
    message: "string",
    object: objectRef,
    conflicts: arr(
      obj<C.RowConflict>({
        index: "number",
        op: lit("update", "delete", "insert"),
        key: arr(field),
        original: arr(field),
        current: nul(arr(field)),
        attempted: arr(field),
      }),
    ),
    compensated: opt("boolean"),
  }),
  "stream-events": arr({
    union: {
      tag: "event",
      cases: {
        statement: { index: "number", sql: "string" },
        columns: { index: "number", columns: arr(columnInfo) },
        rows: { index: "number", rows: arr(arr("value")) },
        done: { index: "number", affected: nul("number"), rows: "number", truncated: "boolean", elapsedMs: "number" },
        notice: { index: "number", text: "string" },
      },
    },
  }),
  "run-request": obj<C.RunRequest>({ text: "string", database: nul("string"), offset: "number" }),
  "run-summary": obj<C.RunSummary>({ statements: "number", elapsedMs: "number", stopped: "boolean", transaction: transactionState }),
  "session-info": obj<C.SessionInfo>({ sessionId: "string", language: "string", transaction: transactionState, cancellable: "boolean" }),
  "session-options": obj<C.SessionOptions>({ language: nul("string"), database: nul("string") }),
  "engine-descriptor": obj<C.EngineDescriptor>({
    id: "string",
    label: "string",
    family: lit("sql", "document", "keyValue", "search"),
    icon: "string",
    status: lit("ready", "preview", "planned"),
    sqlDialect: nul("string"),
    capabilities,
    objectsLabel: "string",
    connection: obj<C.ConnectionForm>({
      usesHost: "boolean",
      usesFile: "boolean",
      defaultPort: nul("number"),
      defaultUser: "string",
      databaseLabel: "string",
      databaseHint: "string",
      url: nul(obj<C.UrlSpec>({ schemes: arr("string"), example: "string" })),
      auth: arr("string"),
      fields: arr(
        obj<C.FieldSpec>({
          key: "string",
          kind: {
            union: {
              tag: "type",
              cases: {
                text: {},
                number: {},
                password: { slot: "string" },
                checkbox: {},
                select: { options: arr({ tuple: ["string", "string"] }) },
                file: { filters: arr("string"), save: "boolean" },
                textarea: {},
              },
            },
          },
          label: "string",
          placeholder: nul("string"),
          hint: nul("string"),
          section: lit("general", "auth", "advanced"),
          default: nul("string"),
          required: "boolean",
        }),
      ),
      ssh: "boolean",
      tls: nul(obj<C.TlsSupport>({ modes: arr("string"), ca: "boolean", clientCert: "boolean", serverName: "boolean", pin: "boolean" })),
    }),
    languages: arr(languageSpec),
    filter: obj<C.FilterSupport>({ structured: "boolean", raw: nul("string"), placeholder: "string" }),
    assistant: obj<C.AssistantHints>({
      testData: "string",
      prompts: arr(obj<C.AssistantPrompt>({ label: "string", icon: "string", prompt: "string" })),
      translateFrom: "boolean",
      translateTo: nul("string"),
      translateLabel: nul("string"),
    }),
  }),
  "connect-info": obj<C.ConnectInfo>({
    engine: "string",
    server: obj<C.ServerInfo>({ product: "string", version: "string", label: "string", flavor: nul("string") }),
    capabilities,
    languages: arr(languageSpec),
    connectionActions: arr(nodeAction),
  }),
  "connection-spec": obj<C.ConnectionSpec>({
    engine: "string",
    host: "string",
    port: "number",
    user: "string",
    database: nul("string"),
    file: nul("string"),
    url: nul("string"),
    options: { record: "string" },
    tls: nul(
      obj<C.TlsConfig>({
        mode: "string",
        caFile: nul("string"),
        clientCertFile: nul("string"),
        clientKeyFile: nul("string"),
        serverName: nul("string"),
        acceptInvalidCerts: "boolean",
        pinnedSha256: nul("string"),
      }),
    ),
    ssh: nul("any"),
    auth: nul("string"),
  }),
};

const fixtures = import.meta.glob<unknown>("./__fixtures__/contract/*.json", { eager: true, import: "default" });

describe("contract fixtures", () => {
  it("has a shape for every fixture and a fixture for every shape", () => {
    const names = Object.keys(fixtures).map((path) => path.replace(/^.*\//, "").replace(/\.json$/, ""));
    expect(names.sort()).toEqual(Object.keys(SHAPES).sort());
  });

  for (const [name, shape] of Object.entries(SHAPES)) {
    it(`${name} matches the TypeScript types`, () => {
      const errors: string[] = [];
      check(fixtures[`./__fixtures__/contract/${name}.json`], shape, name, errors);
      expect(errors).toEqual([]);
    });
  }
});
