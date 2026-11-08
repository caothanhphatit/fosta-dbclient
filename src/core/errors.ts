/**
 * One shape for every error the UI shows: the backend's structured `AppError` (`code`, `message`, engine detail, position,
 * failing statement, …). Anything else that is thrown (a string from the web transport, an `Error`, a plain object) is
 * normalised to it here, so views never inspect raw rejections themselves. Pure functions; the DOM side is `errorPanel.ts`.
 */
import type { AppError, ErrorPosition } from "./contract";
import type { Diagnostics } from "./types";

/** The code of an error that carries none (the backend's `GENERIC`). */
export const GENERIC_CODE = "ERROR";

/** Fields `formatError` and the error block show by name; everything else is listed as extra JSON. */
const KNOWN_FIELDS = new Set(["code", "message", "engine", "detail", "hint", "sqlstate", "vendorCode", "position", "statement", "cause", "retryable"]);

/** Normalises any thrown value to an `AppError` (never throws). */
export function toAppError(error: unknown): AppError {
  if (error === null || error === undefined) return { code: GENERIC_CODE, message: "Unknown error" };
  if (typeof error === "string") return { code: GENERIC_CODE, message: error };
  if (typeof error !== "object") return { code: GENERIC_CODE, message: String(error) };
  const source = error as Record<string, unknown>;
  const fields: Record<string, unknown> = {};
  // Own enumerable properties: a backend object, or an Error with fields assigned to it (the dev mock does that).
  for (const [key, value] of Object.entries(source)) if (value !== undefined) fields[key] = value;
  const message =
    typeof source.message === "string" && source.message
      ? source.message
      : error instanceof Error
        ? error.message || error.name
        : Object.keys(fields).length > 0
          ? JSON.stringify(fields)
          : "Unknown error";
  const code = typeof source.code === "string" && source.code ? source.code : GENERIC_CODE;
  const normalised: AppError = { ...fields, code, message };
  if (normalised.position !== undefined && !isPosition(normalised.position)) delete normalised.position;
  if (normalised.statement !== undefined && !isStatement(normalised.statement)) delete normalised.statement;
  if (normalised.cause !== undefined && !Array.isArray(normalised.cause)) normalised.cause = [String(normalised.cause)];
  return normalised;
}

function isPosition(value: unknown): value is ErrorPosition {
  return typeof value === "object" && value !== null && ["line", "column", "offset"].some((key) => typeof (value as Record<string, unknown>)[key] === "number");
}

function isStatement(value: unknown): value is { index: number; text: string } {
  return typeof value === "object" && value !== null && typeof (value as { text?: unknown }).text === "string";
}

/** The structured code of an error, or null when it carries none (a plain string or a bare `Error`). */
export function errorCode(error: unknown): string | null {
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") return error.code;
  return null;
}

/** The message of any thrown value. */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  return toAppError(error).message;
}

/** "line 3, column 7" (plus the offset when known), or null without a position. */
export function describePosition(position: ErrorPosition | undefined): string | null {
  if (!position) return null;
  const parts: string[] = [];
  if (position.line !== undefined) parts.push(`line ${position.line}`);
  if (position.column !== undefined) parts.push(`column ${position.column}`);
  if (position.offset !== undefined) parts.push(parts.length > 0 ? `(offset ${position.offset})` : `offset ${position.offset}`);
  return parts.length > 0 ? parts.join(", ").replace(", (", " (") : null;
}

/** "SQLSTATE 42000 · vendor code 1064", or null when the engine reported neither. */
export function describeCodes(error: AppError): string | null {
  const parts: string[] = [];
  if (error.sqlstate) parts.push(`SQLSTATE ${error.sqlstate}`);
  if (error.vendorCode !== undefined && error.vendorCode !== null && error.vendorCode !== "") parts.push(`vendor code ${error.vendorCode}`);
  return parts.length > 0 ? parts.join(" · ") : null;
}

/** Fields not shown by name (gateway problems, conflicts, `kind`, …). */
export function extraFields(error: AppError): Record<string, unknown> {
  return Object.fromEntries(Object.entries(error).filter(([key]) => !KNOWN_FIELDS.has(key)));
}

/** Plain text for a bug report: versions, open connections and the newest errors (no secrets, no row data). */
export function diagnosticsText(d: Diagnostics): string {
  const lines = [
    `Fosta Studio ${d.appVersion} (${d.edition})`,
    `OS: ${d.os} ${d.arch}${d.osVersion ? ` · ${d.osVersion}` : ""}`,
    `Log folder: ${d.logFolder ?? "not set up"}${d.debugLogging ? " (debug logging on)" : ""}`,
    `Open connections: ${d.connections.length === 0 ? "none" : d.connections.map((c) => `${c.engine} ${c.version}${c.crashed ? " (crashed)" : ""}`).join(", ")}`,
  ];
  if (d.recentErrors.length > 0) {
    lines.push("Recent errors:");
    for (const error of d.recentErrors.slice(-20)) lines.push(`  ${error.time} ${error.level} ${error.code ?? ""} ${error.target}: ${error.message}`);
  }
  return lines.join("\n");
}

export interface FormatOptions {
  /** What the user was doing ("Connect to Local MariaDB", "Save rows of shop.products"). */
  context?: string;
  /** Prepended as a header so a pasted report says which build and OS it came from. */
  diagnostics?: Diagnostics | null;
  /** When the error happened; defaults to none. */
  time?: Date;
}

/** The "Copy error" text: every field of the error, then the diagnostics. */
export function formatError(error: unknown, options: FormatOptions = {}): string {
  const e = toAppError(error);
  const lines = [`Error ${e.code}: ${e.message}`];
  if (options.time) lines.push(`Time: ${options.time.toISOString()}`);
  if (options.context) lines.push(`While: ${options.context}`);
  if (e.engine) lines.push(`Engine: ${e.engine}`);
  const codes = describeCodes(e);
  if (codes) lines.push(`Codes: ${codes}`);
  if (e.detail) lines.push(`Detail: ${e.detail}`);
  if (e.hint) lines.push(`Hint: ${e.hint}`);
  const position = describePosition(e.position);
  if (position) lines.push(`Position: ${position}`);
  if (e.statement) lines.push(`Statement ${e.statement.index + 1}: ${e.statement.text}`);
  if (e.cause && e.cause.length > 0) lines.push(`Cause: ${e.cause.join(" <- ")}`);
  if (e.retryable !== undefined) lines.push(`Retryable: ${e.retryable ? "yes" : "no"}`);
  const extra = extraFields(e);
  if (Object.keys(extra).length > 0) lines.push(`Other fields: ${JSON.stringify(extra, null, 2)}`);
  if (options.diagnostics) lines.push("", "--- Diagnostics ---", diagnosticsText(options.diagnostics));
  return lines.join("\n");
}

/**
 * The editor offset of an error position inside `text` (the text that ran): the offset when known, else line and
 * column (both 1-based). Null when the position is outside the text.
 */
export function positionOffset(text: string, position: ErrorPosition): number | null {
  if (position.offset !== undefined) return position.offset >= 0 && position.offset <= text.length ? position.offset : null;
  if (position.line === undefined) return null;
  let offset = 0;
  for (let line = 1; line < position.line; line += 1) {
    const newline = text.indexOf("\n", offset);
    if (newline < 0) return null;
    offset = newline + 1;
  }
  const end = text.indexOf("\n", offset);
  const lineLength = (end < 0 ? text.length : end) - offset;
  return offset + Math.min(Math.max((position.column ?? 1) - 1, 0), lineLength);
}
