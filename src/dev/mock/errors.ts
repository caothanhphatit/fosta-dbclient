/**
 * Errors of the mock in the backend's wire shape (a plain object, as Tauri rejects with): `code`, `message` and the
 * structured fields of `AppError`. Never `new Error(…)`, so the UI sees exactly what the desktop app would send.
 */
import type { AppError } from "../../core/contract";

export function mockError(code: string, message: string, fields: Omit<AppError, "code" | "message"> = {}): AppError {
  return { code, message, ...fields };
}

/** Line, column (1-based) and offset (0-based) of `offset` in `text`, like Rust `ErrorPosition::at_offset`. */
export function positionAt(text: string, offset: number): { line: number; column: number; offset: number } {
  const before = text.slice(0, offset);
  const lines = before.split("\n");
  return { line: lines.length, column: lines[lines.length - 1].length + 1, offset };
}

export const NOT_CONNECTED = "Not connected. Double-click the connection to connect first.";
