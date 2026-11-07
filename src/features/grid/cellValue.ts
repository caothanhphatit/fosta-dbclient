/**
 * Pure helpers for the cell editor: JSON detection, formatting and validation, text statistics and a
 * hex dump of binary values (which travel as `0x…` text).
 */

export interface JsonCheck {
  valid: boolean;
  /** Parser message without the position suffix. */
  message: string;
  /** Zero-based character offset of the problem, when the parser reports one. */
  position: number | null;
  /** One-based line and column of `position`. */
  line: number | null;
  column: number | null;
}

/** Text that is meant to be a JSON object or array (the editor then offers Format / Minify). */
export function looksLikeJson(text: string): boolean {
  return /^\s*[[{]/.test(text);
}

export function formatJson(text: string, indent = 2): string {
  return JSON.stringify(JSON.parse(text), null, indent);
}

export function minifyJson(text: string): string {
  return JSON.stringify(JSON.parse(text));
}

export function checkJson(text: string): JsonCheck {
  try {
    JSON.parse(text);
    return { valid: true, message: "", position: null, line: null, column: null };
  } catch (error) {
    return describeJsonError(text, error instanceof Error ? error.message : String(error));
  }
}

/** Extracts the position from V8 ("at position 12"), Firefox ("at line 2 column 5") or WebKit messages. */
export function describeJsonError(text: string, raw: string): JsonCheck {
  const message = raw
    .replace(/^JSON\.parse:\s*/, "")
    .replace(/\s*\(line \d+ column \d+\)$/, "")
    .replace(/\s+(in JSON )?at position \d+.*$/, "")
    .replace(/\s+at line \d+ column \d+ of the JSON data$/, "");
  const offset = /at position (\d+)/.exec(raw);
  if (offset) {
    const position = Number(offset[1]);
    return { valid: false, message, position, ...lineColumn(text, position) };
  }
  const lineMatch = /line (\d+) column (\d+)/.exec(raw);
  if (lineMatch) {
    const line = Number(lineMatch[1]);
    const column = Number(lineMatch[2]);
    return { valid: false, message, position: offsetOf(text, line, column), line, column };
  }
  if (/end of (JSON )?(input|data)/i.test(raw)) {
    return { valid: false, message, position: text.length, ...lineColumn(text, text.length) };
  }
  return { valid: false, message, position: null, line: null, column: null };
}

export function lineColumn(text: string, position: number): { line: number; column: number } {
  const before = text.slice(0, Math.max(0, position));
  const lines = before.split("\n");
  return { line: lines.length, column: lines[lines.length - 1].length + 1 };
}

function offsetOf(text: string, line: number, column: number): number {
  const lines = text.split("\n");
  let offset = 0;
  for (let index = 0; index < line - 1 && index < lines.length; index += 1) offset += lines[index].length + 1;
  return offset + column - 1;
}

/** "3 lines · 1,204 chars" style counts. */
export function textStats(text: string): { lines: number; chars: number } {
  return { lines: text === "" ? 1 : text.split("\n").length, chars: [...text].length };
}

/** Bytes of a `0x…` hex value as an offset / hex / ASCII dump, 16 bytes per line. */
export function hexDump(value: string, bytesPerLine = 16): string {
  const hex = value.replace(/^0x/i, "").replace(/\s+/g, "");
  if (!/^[0-9a-f]*$/i.test(hex) || hex.length % 2 !== 0) return value;
  const bytes = hex.match(/../g)?.map((pair) => parseInt(pair, 16)) ?? [];
  if (bytes.length === 0) return "(empty)";
  const lines: string[] = [];
  for (let start = 0; start < bytes.length; start += bytesPerLine) {
    const chunk = bytes.slice(start, start + bytesPerLine);
    const hexPart = chunk.map((byte) => byte.toString(16).padStart(2, "0")).join(" ");
    const ascii = chunk.map((byte) => (byte >= 0x20 && byte < 0x7f ? String.fromCharCode(byte) : ".")).join("");
    lines.push(`${start.toString(16).padStart(8, "0")}  ${hexPart.padEnd(bytesPerLine * 3 - 1)}  ${ascii}`);
  }
  return lines.join("\n");
}

/** Vector components, one per line with their index. */
export function vectorListing(values: number[]): string {
  const width = String(values.length - 1).length;
  return values.map((value, index) => `${String(index).padStart(width)}  ${value}`).join("\n");
}
