import type { Cell } from "../../core/types";

export const FETCH_PAGE = 1000;

/** Thrown when the user cancels an export; the dialog swallows it. */
export class ExportCancelled extends Error {
  constructor() {
    super("Export cancelled");
    this.name = "ExportCancelled";
  }
}

/**
 * Reads every row page by page. Checks `signal` before each page and after it arrives, so closing the dialog
 * (Esc or Cancel) stops the export instead of letting it finish in the background.
 */
export async function fetchAllRows(
  fetchPage: (offset: number, limit: number) => Promise<Cell[][]>,
  total: number,
  signal: AbortSignal,
  onProgress: (done: number) => void = () => undefined,
): Promise<Cell[][]> {
  const rows: Cell[][] = [];
  for (let offset = 0; offset < total || offset === 0; offset += FETCH_PAGE) {
    if (signal.aborted) throw new ExportCancelled();
    const page = await fetchPage(offset, FETCH_PAGE);
    if (signal.aborted) throw new ExportCancelled();
    rows.push(...page);
    onProgress(rows.length);
    if (page.length < FETCH_PAGE) break;
  }
  return rows;
}

/** The file name to save under: the typed one, or the table name with the format's extension. */
export function exportFileName(typed: string, table: string, extension: string): string {
  return typed.trim() || `${table}.${extension}`;
}
