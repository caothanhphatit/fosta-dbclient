/**
 * The shapes of the `redis.*` operations (`src-tauri/src/engines/redis/ops.rs`) and the pure helpers the Redis views share:
 * binary-safe key and value bytes, TTL text and parsing, key-list patterns, and the `expect` guards of edits.
 */

/** Binary-safe bytes: `text` when valid UTF-8, `base64` otherwise (the backend's `Blob`). */
export interface Blob {
  text?: string;
  base64?: string;
}

export type RedisType = "string" | "list" | "set" | "zset" | "hash" | "stream" | "json" | "timeSeries" | "vectorset" | "none" | { other: string };

export interface KeyRow {
  key: Blob;
  keyType: RedisType | null;
  /** Milliseconds to live; -1 without expiry, -2 when the key vanished. */
  ttlMs: number | null;
  memoryBytes: number | null;
}

export interface KeyPage {
  cursor: string;
  done: boolean;
  keys: KeyRow[];
  calls: number;
  dbSize: number | null;
}

export interface KeyInfo {
  key: Blob;
  keyType: RedisType;
  ttlMs: number;
  memoryBytes: number | null;
  encoding: string | null;
  length: number | null;
}

export type StringFormat = "text" | "json" | "binary";

export interface StringValue {
  value: Blob;
  length: number;
  truncated: boolean;
  format: StringFormat;
  json: unknown;
}

export interface FieldValue {
  field: Blob;
  value: Blob;
}

export interface CursorPage<T> {
  items: T[];
  cursor: string;
  done: boolean;
  total: number;
}

export interface ListPage {
  start: number;
  items: Blob[];
  total: number;
}

export interface ScoredMember {
  member: Blob;
  score: number;
}

export interface ZsetPage {
  items: ScoredMember[];
  offset: number;
  total: number;
}

export interface StreamEntry {
  id: string;
  fields: FieldValue[];
}

export interface StreamPage {
  entries: StreamEntry[];
  next: string | null;
  length: number;
}

export interface ConsumerGroup {
  name: string;
  consumers: number;
  pending: number;
  lastDeliveredId: string;
  lag: number | null;
}

export interface StreamInfo {
  length: number;
  firstId: string | null;
  lastId: string | null;
  groups: ConsumerGroup[];
}

export type ValuePayload =
  | { type: "string"; data: StringValue }
  | { type: "hash"; data: CursorPage<FieldValue> }
  | { type: "list"; data: ListPage }
  | { type: "set"; data: CursorPage<Blob> }
  | { type: "zset"; data: ZsetPage }
  | { type: "stream"; data: { page: StreamPage; info: StreamInfo } }
  | { type: "json"; data: { value: unknown; jsonType: string | null; memoryBytes: number | null } }
  | { type: "none" }
  | { type: "unsupported"; data: { typeName: string } };

export interface ValueRead {
  info: KeyInfo;
  value: ValuePayload;
}

export interface ReadOptions {
  cursor?: string;
  pattern?: string;
  count?: number;
  /** First index of a list window. */
  start?: number;
  zset?: { by: "rank"; start: number; count?: number; rev: boolean };
  stream?: { after?: string | null; before?: string | null; count?: number; reverse: boolean };
}

/** What an edit depends on (`guard.rs`): the part of the key as the user saw it. */
export type Expect =
  | { kind: "string"; value: Blob | null }
  | { kind: "hash"; fields: { field: Blob; value?: Blob | null }[] }
  | { kind: "listItem"; index: number; value: Blob | null }
  | { kind: "set"; members: { member: Blob; present: boolean }[] }
  | { kind: "zset"; members: { member: Blob; score: number | null }[] }
  | { kind: "missing" }
  | { kind: "exists" };

/** A change to one key (`edit.rs`); `db` and `expect` are added by the API. */
export type RedisEdit =
  | { op: "setString"; key: Blob; value: Blob; ttlMs: number | null; keepTtl: boolean; condition: "always" | "ifMissing" | "ifExists" }
  | { op: "expire"; key: Blob; ttlMs: number }
  | { op: "persist"; key: Blob }
  | { op: "delete"; keys: Blob[] }
  | { op: "hashSet"; key: Blob; fields: FieldValue[] }
  | { op: "hashDelete"; key: Blob; fields: Blob[] }
  | { op: "listPush"; key: Blob; values: Blob[]; end: "head" | "tail" }
  | { op: "listSet"; key: Blob; index: number; value: Blob }
  | { op: "listRemove"; key: Blob; value: Blob; count: number }
  | { op: "setAdd"; key: Blob; members: Blob[] }
  | { op: "setRemove"; key: Blob; members: Blob[] }
  | { op: "zsetAdd"; key: Blob; members: ScoredMember[] }
  | { op: "zsetRemove"; key: Blob; members: Blob[] }
  | { op: "streamAdd"; key: Blob; id: string | null; fields: FieldValue[]; maxLen: number | null };

export interface EditResult {
  count: number | null;
  applied: boolean;
}

/** The body of a `CONFLICT` error from `redis.edit`. */
export interface RedisConflict {
  code: "CONFLICT";
  message: string;
  key: Blob;
  expected: Expect;
  current: Expect | null;
}

export interface SlowlogEntry {
  id: number;
  timestamp: number;
  durationUs: number;
  command: string[];
  client: string | null;
  clientName: string | null;
}

export interface Slowlog {
  length: number | null;
  entries: SlowlogEntry[];
  slowerThanUs: string | null;
  maxLen: string | null;
}

export interface ServerInfo {
  version: string;
  flavor: string;
  mode: string;
  role: string;
  os: string | null;
  uptimeSeconds: number | null;
  connectedClients: number | null;
  usedMemory: number | null;
  usedMemoryPeak: number | null;
  maxmemory: number | null;
  maxmemoryPolicy: string | null;
  totalCommandsProcessed: number | null;
  keyspaceHits: number | null;
  keyspaceMisses: number | null;
  databases: number | null;
  currentDb: number;
  keyspace: { db: number; keys: number; expires: number; avgTtlMs: number }[];
  modules: { name: string; version: number | null }[];
  sections: Record<string, Record<string, string>>;
}

// --- Bytes ----------------------------------------------------------------------------------------------------------------

export const textBlob = (text: string): Blob => ({ text });

/** Standard base64 of UTF-8 text or of raw bytes. */
function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function fromBase64(text: string): Uint8Array {
  const binary = atob(text);
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

export const isBinary = (blob: Blob): boolean => blob.text === undefined;

/** What to show for bytes: the text, or `0x…` hex of binary data. */
export function blobDisplay(blob: Blob | null | undefined): string {
  if (!blob) return "";
  if (blob.text !== undefined) return blob.text;
  if (blob.base64 === undefined) return "";
  return `0x${[...fromBase64(blob.base64)].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

/** The blob of a key as the tree and the key list name it: `name` is base64 when `binary`. */
export function keyBlob(name: string, binary: boolean): Blob {
  return binary ? { base64: name } : { text: name };
}

/** The `name` and `binary` flag of an object reference for `key`. */
export function keyIdentity(key: Blob): { name: string; binary: boolean } {
  return key.text !== undefined ? { name: key.text, binary: false } : { name: key.base64 ?? "", binary: true };
}

export function blobBytes(blob: Blob): Uint8Array {
  return blob.text !== undefined ? new TextEncoder().encode(blob.text) : fromBase64(blob.base64 ?? "");
}

export function sameBlob(a: Blob | null | undefined, b: Blob | null | undefined): boolean {
  if (!a || !b) return !a && !b;
  return a.text === b.text && a.base64 === b.base64;
}

export function blobFromBytes(bytes: Uint8Array): Blob {
  try {
    return { text: new TextDecoder("utf-8", { fatal: true }).decode(bytes) };
  } catch {
    return { base64: toBase64(bytes) };
  }
}

// --- Types ----------------------------------------------------------------------------------------------------------------

export function typeName(type: RedisType | null | undefined): string {
  if (!type) return "";
  if (typeof type === "string") return type === "timeSeries" ? "timeseries" : type;
  return type.other;
}

/** Types with a value editor; the rest open read-only with a pointer to the console. */
export const EDITABLE_TYPES: ReadonlySet<string> = new Set(["string", "hash", "list", "set", "zset", "stream"]);

// --- TTL --------------------------------------------------------------------------------------------------------------------

/** `59s`, `4m`, `3h`, `2d`; `no expiry` for -1, `missing` for -2. */
export function formatTtl(ttlMs: number | null | undefined): string {
  if (ttlMs === null || ttlMs === undefined) return "";
  if (ttlMs === -1) return "no expiry";
  if (ttlMs < 0) return "missing";
  const seconds = Math.floor(ttlMs / 1000);
  if (seconds < 1) return `${ttlMs} ms`;
  if (seconds < 120) return `${seconds}s`;
  if (seconds < 7200) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`.replace(/ 0s$/, "");
  if (seconds < 172_800) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`.replace(/ 0m$/, "");
  return `${Math.floor(seconds / 86_400)}d ${Math.floor((seconds % 86_400) / 3600)}h`.replace(/ 0h$/, "");
}

const UNIT_MS: Record<string, number> = { ms: 1, s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000 };

/** `90`, `90s`, `10m`, `1.5h`, `2d`, `500ms` to milliseconds; a bare number is seconds. Null for anything else or <= 0. */
export function parseTtl(text: string): number | null {
  const match = /^\s*(\d+(?:\.\d+)?)\s*(ms|s|m|h|d)?\s*$/i.exec(text);
  if (!match) return null;
  const ms = Math.round(Number(match[1]) * UNIT_MS[(match[2] ?? "s").toLowerCase()]);
  return Number.isFinite(ms) && ms > 0 ? ms : null;
}

// --- Numbers --------------------------------------------------------------------------------------------------------------

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

export function formatDuration(seconds: number): string {
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m ${seconds % 60}s`;
}

/** Microseconds as `850 µs`, `12.4 ms` or `1.20 s`. */
export function formatMicros(micros: number): string {
  if (micros < 1000) return `${micros} µs`;
  if (micros < 1_000_000) return `${(micros / 1000).toFixed(1)} ms`;
  return `${(micros / 1_000_000).toFixed(2)} s`;
}

// --- Key list -------------------------------------------------------------------------------------------------------------

/** The `MATCH` pattern for what the user typed: text without glob characters finds keys that contain it. */
export function matchPattern(input: string): string | undefined {
  const text = input.trim();
  if (!text) return undefined;
  return /[*?[]/.test(text) ? text : `*${text}*`;
}

/** Splits `a:b:c` into folder-like parts for display (`a:b:` and `c`). */
export function splitKey(key: string, delimiter = ":"): { prefix: string; leaf: string } {
  const at = key.lastIndexOf(delimiter);
  return at < 0 ? { prefix: "", leaf: key } : { prefix: key.slice(0, at + 1), leaf: key.slice(at + 1) };
}

// --- Errors ---------------------------------------------------------------------------------------------------------------

/** The `CONFLICT` payload of a thrown error, or null for any other error. */
export function conflictOf(error: unknown): RedisConflict | null {
  if (typeof error !== "object" || error === null) return null;
  const candidate = error as Partial<RedisConflict>;
  return candidate.code === "CONFLICT" && candidate.expected ? (candidate as RedisConflict) : null;
}

/** A readable one-line form of what a conflict found: "now: hello" or "no longer exists". */
export function describeCurrent(conflict: RedisConflict): string {
  const current = conflict.current;
  if (!current) return "The key changed while it was being saved.";
  switch (current.kind) {
    case "string":
      return current.value ? `Now: ${blobDisplay(current.value)}` : "The key no longer exists.";
    case "hash":
      return current.fields.map((field) => `${blobDisplay(field.field)} = ${field.value ? blobDisplay(field.value) : "(missing)"}`).join(", ");
    case "listItem":
      return current.value ? `Item ${current.index} is now: ${blobDisplay(current.value)}` : `Item ${current.index} no longer exists.`;
    case "set":
      return current.members.map((member) => `${blobDisplay(member.member)} ${member.present ? "is" : "is not"} in the set`).join(", ");
    case "zset":
      return current.members.map((member) => `${blobDisplay(member.member)}: ${member.score === null ? "(missing)" : member.score}`).join(", ");
    case "missing":
      return "The key does not exist.";
    case "exists":
      return "The key already exists.";
  }
}
