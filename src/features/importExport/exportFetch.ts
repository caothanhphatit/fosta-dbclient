import type { Page, Value } from "../../core/contract";

export const FETCH_PAGE = 1000;

/** Thrown when the user cancels an export; the dialog swallows it. */
export class ExportCancelled extends Error {
  constructor() {
    super("Export cancelled");
    this.name = "ExportCancelled";
  }
}

/** Where the next page starts: after a keyset (the previous page's `nextKeyset`) or, without one, at an offset. */
export interface PageCursor {
  offset: number;
  after: Value[] | null;
}

export type PageReader = (cursor: PageCursor, limit: number) => Promise<Pick<Page, "rows" | "hasMore" | "nextKeyset">>;

/**
 * Reads every row page by page: by keyset while the engine hands one out (each page is an index seek, so a large export
 * stays linear), by offset otherwise. Checks `signal` before each page and after it arrives, so closing the dialog
 * (Esc or Cancel) stops the export instead of letting it finish in the background.
 */
export async function fetchAllRows(read: PageReader, signal: AbortSignal, onProgress: (done: number) => void = () => undefined): Promise<Value[][]> {
  const rows: Value[][] = [];
  let cursor: PageCursor = { offset: 0, after: null };
  for (;;) {
    if (signal.aborted) throw new ExportCancelled();
    const page = await read(cursor, FETCH_PAGE);
    if (signal.aborted) throw new ExportCancelled();
    rows.push(...page.rows);
    onProgress(rows.length);
    if (!page.hasMore || page.rows.length === 0) break;
    cursor = { offset: rows.length, after: page.nextKeyset ?? null };
  }
  return rows;
}

/** The file name to save under: the typed one, or the table name with the format's extension. */
export function exportFileName(typed: string, table: string, extension: string): string {
  return typed.trim() || `${table}.${extension}`;
}
