/**
 * "Learn this database" in the browser mock: `profile_*` commands over the engine mocks' schema graphs, with
 * `profile-progress` events, Cancel (`ai_cancel`), value hints for a few well-known columns and a scripted summary.
 * The settings gates follow the backend (Settings → Assistant may learn databases, the workspace override, reads).
 *
 * `__fostaMock.profileStale(connectionId, database)` marks a learned profile stale, so the panel and the tab show
 * "Schema changed — refresh".
 */
import { emit } from "@tauri-apps/api/event";
import type { SchemaGraph, ValueKind } from "../../core/contract";
import type {
  DatabaseProfile,
  LearnOutcome,
  ObjectProfile,
  ProfileNote,
  ProfileProgress,
  ProfileStatus,
  SummaryCost,
  SummaryOutcome,
  ValueHint,
} from "../../core/profileTypes";
import { learningAllowed, learningRefusal, qualifiedName, readsAllowed } from "../../features/assistant/profileLogic";
import { mockError } from "./errors";
import { state } from "./state";

const stored = new Map<string, DatabaseProfile>();
const staleKeys = new Set<string>();
const runs = new Map<string, AbortController>();

const key = (connectionId: string, database: string) => `${connectionId}\u0000${database}`;

/** Stops a running build or summary (`ai_cancel`); false when `id` is not one. */
export function cancelProfileRun(id: string): boolean {
  const run = runs.get(id);
  run?.abort();
  return Boolean(run);
}

export function markProfileStale(connectionId: string, database: string): boolean {
  if (!stored.has(key(connectionId, database))) return false;
  staleKeys.add(key(connectionId, database));
  return true;
}

function checkLearning(connectionId: string): void {
  if (!learningAllowed(state.config, connectionId)) throw mockError("ERROR", learningRefusal(state.config, connectionId));
}

const pause = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    if (signal.aborted) return reject(mockError("ERROR", "Stopped"));
    const timer = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(mockError("ERROR", "Stopped"));
    });
  });

function kindOf(type: string): ValueKind {
  const lower = type.toLowerCase();
  if (/int|serial/.test(lower)) return { kind: "int" };
  if (/decimal|numeric|money/.test(lower)) return { kind: "decimal" };
  if (/float|double|real/.test(lower)) return { kind: "float" };
  if (/bool|bit/.test(lower)) return { kind: "bool" };
  if (/timestamp|datetime/.test(lower)) return { kind: "timestamp" };
  if (/date/.test(lower)) return { kind: "date" };
  if (/json|object|document/.test(lower)) return { kind: "json" };
  return { kind: "text" };
}

const SENSITIVE = /password|passwd|secret|token|api_?key|private|salt|hash|ssn|card|cvv|iban/i;

/** Scripted value hints for columns whose names say what they hold. */
function hintFor(column: string): ValueHint | null {
  const name = column.toLowerCase();
  if (name === "status") return { column, values: ["pending", "paid", "shipped", "cancelled"] };
  if (name === "country" || name === "country_code") return { column, values: ["DE", "FR", "GB", "US", "VN"] };
  if (name === "currency") return { column, values: ["EUR", "USD"] };
  if (name.includes("email")) return { column, format: "email address" };
  if (name.endsWith("_at") || name.includes("date")) return { column, format: "ISO date and time" };
  return null;
}

function buildObjects(graph: SchemaGraph, sample: boolean): ObjectProfile[] {
  return graph.tables.map((table) => {
    const dot = table.name.indexOf(".");
    const schema = dot > 0 ? table.name.slice(0, dot) : undefined;
    const name = dot > 0 ? table.name.slice(dot + 1) : table.name;
    const columns = table.columns.map((column) => ({
      name: column.name,
      typeName: column.columnType,
      kind: kindOf(column.columnType),
      nullable: column.nullable,
      primary: column.primary || undefined,
      sensitive: SENSITIVE.test(column.name) || undefined,
    }));
    const hints = sample ? columns.filter((column) => !column.sensitive).flatMap((column) => hintFor(column.name) ?? []) : [];
    return {
      schema,
      name,
      kind: table.kind,
      fingerprint: `${table.name}:${table.columns.map((column) => column.name).join(",")}`,
      detailed: true,
      comment: table.comment || undefined,
      columns,
      primaryKey: columns.filter((column) => column.primary).map((column) => column.name),
      rowEstimate: table.rows ?? undefined,
      valueHints: hints.length ? hints : undefined,
    };
  });
}

function statusOf(profile: DatabaseProfile | undefined, connected: boolean): ProfileStatus {
  if (!profile) return { exists: false, learnedAt: null, objects: 0, detailedObjects: 0, dataRead: false, stale: null, checkedAt: null };
  return {
    exists: true,
    learnedAt: profile.learnedAt,
    objects: profile.totalObjects,
    detailedObjects: profile.detailedObjects,
    dataRead: profile.dataRead,
    stale: connected ? staleKeys.has(key(profile.connectionId, profile.database)) : null,
    checkedAt: connected ? Date.now() : null,
  };
}

export interface LearnSource {
  engine: string;
  graph: SchemaGraph | null;
  connected: boolean;
}

export async function profileLearn(
  connectionId: string,
  database: string,
  runId: string,
  sampleValues: boolean | null,
  source: LearnSource,
): Promise<LearnOutcome> {
  checkLearning(connectionId);
  const controller = new AbortController();
  runs.set(runId, controller);
  const progress = (step: ProfileProgress["step"], done: number, total: number, object: string | null = null) =>
    void emit("profile-progress", { runId, connectionId, database, step, done, total, object } satisfies ProfileProgress);
  try {
    const graph = source.graph ?? { tables: [], relations: [], totalTables: 0 };
    progress("listing", 0, 0);
    await pause(300, controller.signal);
    const total = graph.tables.length;
    for (let index = 0; index < total; index += 1) {
      progress("describing", index, total, graph.tables[index].name);
      await pause(140, controller.signal);
    }
    progress("describing", total, total);
    progress("relations", 0, 0);
    await pause(250, controller.signal);
    const sample = sampleValues !== false && readsAllowed(state.config, connectionId);
    let objects = buildObjects(graph, sample);
    if (sample) {
      for (let index = 0; index < objects.length; index += 1) {
        progress("values", index, objects.length, qualifiedName(objects[index]));
        await pause(90, controller.signal);
      }
    }
    progress("saving", 0, 0);
    await pause(150, controller.signal);
    const before = stored.get(key(connectionId, database));
    // The user's notes and descriptions are kept; the model's are kept while the object did not change.
    objects = objects.map((object) => {
      const old = before?.objects.find((item) => qualifiedName(item) === qualifiedName(object));
      return old?.description && (old.description.source === "user" || old.fingerprint === object.fingerprint)
        ? { ...object, description: old.description }
        : object;
    });
    const names = (profile: DatabaseProfile | undefined) => new Map((profile?.objects ?? []).map((object) => [qualifiedName(object), object.fingerprint]));
    const was = names(before);
    const now = new Map(objects.map((object) => [qualifiedName(object), object.fingerprint]));
    const diff = {
      added: [...now.keys()].filter((name) => !was.has(name)),
      changed: [...now.keys()].filter((name) => was.has(name) && was.get(name) !== now.get(name)),
      removed: [...was.keys()].filter((name) => !now.has(name)),
      unchanged: [...now.keys()].filter((name) => was.get(name) === now.get(name)).length,
    };
    const profile: DatabaseProfile = {
      version: 1,
      engine: source.engine,
      connectionId,
      database,
      fingerprint: [...now.values()].join("|"),
      firstLearnedAt: before?.firstLearnedAt ?? Date.now(),
      learnedAt: Date.now(),
      dataRead: sample,
      model: before?.model,
      inputTokens: before?.inputTokens ?? 0,
      outputTokens: before?.outputTokens ?? 0,
      objects,
      totalObjects: graph.totalTables,
      detailedObjects: objects.length,
      truncated: graph.totalTables > objects.length,
      excludedObjects: 0,
      relations: graph.relations.map((relation) => ({
        name: relation.constraint,
        from: relation.table,
        columns: relation.columns,
        to: relation.referencedTable,
        toColumns: relation.referencedColumns,
        toDatabase: relation.referencedDatabase && relation.referencedDatabase !== relation.database ? relation.referencedDatabase : undefined,
        source: "fk",
        confidence: 1,
      })),
      routines: [],
      summary: before?.summary,
      notes: before?.notes ?? [],
    };
    stored.set(key(connectionId, database), profile);
    staleKeys.delete(key(connectionId, database));
    const hinted = objects.reduce((sum, object) => sum + (object.valueHints?.length ?? 0), 0);
    const columns = objects.reduce(
      (sum, object) => sum + object.columns.filter((column) => !column.sensitive && kindOf(column.typeName).kind === "text").length,
      0,
    );
    return {
      status: statusOf(profile, source.connected),
      refreshed: Boolean(before),
      diff,
      values: sample ? { columns, hinted, failed: 0 } : undefined,
    };
  } finally {
    runs.delete(runId);
  }
}

export function profileGet(connectionId: string, database: string): DatabaseProfile | null {
  return stored.get(key(connectionId, database)) ?? null;
}

export function profileStatus(connectionId: string, database: string, connected: boolean): ProfileStatus {
  return statusOf(stored.get(key(connectionId, database)), connected);
}

function mustGet(connectionId: string, database: string): DatabaseProfile {
  const profile = stored.get(key(connectionId, database));
  if (!profile) throw mockError("ERROR", "The assistant has not learned this database yet");
  return profile;
}

export function profileUpdateNotes(connectionId: string, database: string, notes: ProfileNote[]): DatabaseProfile {
  const profile = { ...mustGet(connectionId, database), notes: notes.map((note) => ({ ...note, text: note.text.slice(0, 2000) })) };
  stored.set(key(connectionId, database), profile);
  return profile;
}

export function profileUpdateDescription(connectionId: string, database: string, object: string, description: string | null): DatabaseProfile {
  const profile = mustGet(connectionId, database);
  if (!profile.objects.some((item) => qualifiedName(item) === object || item.name === object)) throw mockError("ERROR", `No object ${object} was learned`);
  const text = description?.trim().slice(0, 200) ?? "";
  const updated: DatabaseProfile = {
    ...profile,
    objects: profile.objects.map((item) =>
      qualifiedName(item) === object || item.name === object
        ? { ...item, description: text ? { text, source: "user", updatedAt: Date.now() } : undefined }
        : item,
    ),
  };
  stored.set(key(connectionId, database), updated);
  return updated;
}

export function profileDelete(connectionId: string, database: string): boolean {
  staleKeys.delete(key(connectionId, database));
  return stored.delete(key(connectionId, database));
}

function activeModel(model: string | null): { model: string; provider: SummaryCost["provider"] } {
  const active = state.config.aiActive;
  const connection = state.config.aiConnections.find((item) => item.id === active?.connectionId);
  if (!active || !connection) throw mockError("ERROR", "Add an LLM provider first (Assistant → Set up…)");
  return { model: model?.trim() || active.model || connection.defaultModel, provider: connection.provider };
}

const needsDescription = (object: ObjectProfile, onlyChanged: boolean) =>
  object.description?.source !== "user" && (!onlyChanged || !object.description || object.description.fingerprint !== object.fingerprint);

export function profileSummaryEstimate(connectionId: string, database: string, model: string | null, onlyChanged: boolean): SummaryCost {
  const { model: chosen, provider } = activeModel(model);
  const profile = mustGet(connectionId, database);
  const objects = profile.objects.filter((object) => needsDescription(object, onlyChanged));
  const columns = objects.reduce((sum, object) => sum + object.columns.length, 0);
  return {
    objects: objects.length,
    requests: Math.ceil(objects.length / 50) + 1,
    inputTokens: 900 + objects.length * 40 + columns * 9,
    outputTokens: 180 + objects.length * 22,
    dataRead: profile.dataRead && readsAllowed(state.config, connectionId),
    model: chosen,
    provider,
    price: null,
  };
}

/** Scripted descriptions for the sample databases' tables. */
function describe(object: ObjectProfile): string {
  const name = object.name.toLowerCase();
  const known: Record<string, string> = {
    customers: "People and companies who place orders, with contact details.",
    orders: "One row per order: customer, status and totals.",
    order_items: "Lines of each order: product, quantity and price.",
    products: "The catalogue: names, prices and stock.",
    categories: "Groups that products belong to.",
    payments: "Payments received against orders.",
    users: "Accounts that sign in to the application.",
    reviews: "Product reviews written by customers.",
  };
  return known[name] ?? `Holds ${name.replace(/_/g, " ")} records (${object.columns.length} columns).`;
}

export async function profileSummarize(
  connectionId: string,
  database: string,
  runId: string,
  model: string | null,
  onlyChanged: boolean,
): Promise<SummaryOutcome> {
  checkLearning(connectionId);
  const cost = profileSummaryEstimate(connectionId, database, model, onlyChanged);
  const controller = new AbortController();
  runs.set(runId, controller);
  const progress = (done: number, total: number, object: string | null = null) =>
    void emit("profile-progress", { runId, connectionId, database, step: "summary", done, total, object } satisfies ProfileProgress);
  let described = 0;
  try {
    const targets = mustGet(connectionId, database).objects.filter((object) => needsDescription(object, onlyChanged));
    const total = targets.length + 1;
    for (let index = 0; index < targets.length; index += 1) {
      progress(index, total, qualifiedName(targets[index]));
      await pause(120, controller.signal);
      const profile = mustGet(connectionId, database);
      const name = qualifiedName(targets[index]);
      stored.set(key(connectionId, database), {
        ...profile,
        objects: profile.objects.map((object) =>
          qualifiedName(object) === name
            ? { ...object, description: { text: describe(object), source: "llm", updatedAt: Date.now(), fingerprint: object.fingerprint } }
            : object,
        ),
      });
      described += 1;
    }
    progress(targets.length, total, null);
    await pause(400, controller.signal);
    const profile = mustGet(connectionId, database);
    const names = profile.objects.slice(0, 4).map(qualifiedName).join(", ");
    const summary = `${database || "This database"} holds ${profile.totalObjects} objects${names ? ` such as ${names}` : ""}. ${profile.relations.length} foreign keys link them; orders reference customers and products.`;
    stored.set(key(connectionId, database), {
      ...profile,
      model: cost.model,
      inputTokens: profile.inputTokens + cost.inputTokens,
      outputTokens: profile.outputTokens + cost.outputTokens,
      summary: { text: summary, model: cost.model, createdAt: Date.now() },
    });
    progress(total, total, null);
    return {
      objects: targets.length,
      described,
      requests: cost.requests,
      inputTokens: cost.inputTokens,
      outputTokens: cost.outputTokens,
      model: cost.model,
      dataRead: cost.dataRead,
      summary,
    };
  } finally {
    runs.delete(runId);
  }
}
