/**
 * What a streamed `run` produced, built from its `StreamEvent`s: one entry per statement or result set, in arrival
 * order. Pure (no DOM), so the query view's accumulation and its error-position mapping are unit-tested.
 */
import type { ColumnInfo, ErrorPosition, StreamEvent, Value } from "../../core/contract";
import { positionOffset } from "../../core/errors";
import type { HistoryResult } from "../../core/types";

export interface StatementDone {
  affected: number | null;
  rows: number;
  truncated: boolean;
  elapsedMs: number;
}

export interface StatementResult {
  /** `StreamEvent.index`: the statement (or, for a SQL Server batch, the result) it belongs to. */
  index: number;
  sql: string;
  /** Null for a statement without a result set (writes, DDL, USE). */
  columns: ColumnInfo[] | null;
  rows: Value[][];
  notices: string[];
  done: StatementDone | null;
}

export class RunResults {
  readonly entries: StatementResult[] = [];
  /** The latest entry for each index: rows, `done` and notices go there. */
  private readonly latest = new Map<number, StatementResult>();

  /** Adds one event; returns the entry it changed. */
  apply(event: StreamEvent): StatementResult {
    switch (event.event) {
      case "statement":
        return this.start(event.index, event.sql);
      case "columns": {
        // A second result set for the same index (a procedure returning several) gets an entry of its own.
        const current = this.latest.get(event.index);
        const entry = current && current.columns === null && current.done === null ? current : this.start(event.index, current?.sql ?? "");
        entry.columns = event.columns;
        return entry;
      }
      case "rows": {
        const entry = this.entry(event.index);
        for (const row of event.rows) entry.rows.push(row);
        return entry;
      }
      case "done": {
        const entry = this.entry(event.index);
        entry.done = { affected: event.affected, rows: event.rows, truncated: event.truncated, elapsedMs: event.elapsedMs };
        return entry;
      }
      case "notice": {
        const entry = this.entry(event.index);
        entry.notices.push(event.text);
        return entry;
      }
    }
  }

  /** Entries with a result set, in order. */
  resultSets(): StatementResult[] {
    return this.entries.filter((entry) => entry.columns !== null);
  }

  /** Rows received so far, over every result set. */
  rowCount(): number {
    return this.entries.reduce((total, entry) => total + entry.rows.length, 0);
  }

  /** The run for the query history: rows per result set or affected rows per statement. */
  history(): HistoryResult[] {
    return this.entries.map((entry) => ({
      rowSet: entry.columns !== null,
      rows: entry.columns !== null ? (entry.done?.rows ?? entry.rows.length) : 0,
      affectedRows: entry.columns !== null ? 0 : (entry.done?.affected ?? 0),
      truncated: entry.done?.truncated ?? false,
    }));
  }

  private start(index: number, sql: string): StatementResult {
    const entry: StatementResult = { index, sql, columns: null, rows: [], notices: [], done: null };
    this.entries.push(entry);
    this.latest.set(index, entry);
    return entry;
  }

  private entry(index: number): StatementResult {
    return this.latest.get(index) ?? this.start(index, "");
  }
}

const numbers = new Intl.NumberFormat("en-US");

/**
 * One line for the Messages panel: "#2: 3 row(s) affected · 4 ms", "#1: 1,000 row(s) (truncated: …)"; `unfinished` for a
 * statement without its `done` event (still running, or the one that failed).
 */
export function describeResult(entry: StatementResult, position: number, unfinished = "running…"): string {
  const label = `#${position + 1}`;
  const done = entry.done;
  const time = done ? ` · ${numbers.format(done.elapsedMs)} ms` : "";
  if (entry.columns !== null) {
    const rows = done?.rows ?? entry.rows.length;
    const limited = done?.truncated ? ` (truncated: only the first ${numbers.format(rows)} were read)` : "";
    return `${label}: ${numbers.format(rows)} row(s)${limited}${time}`;
  }
  if (!done) return `${label}: ${unfinished}`;
  if (done.affected !== null) return `${label}: ${numbers.format(done.affected)} row(s) affected${time}`;
  return `${label}: OK${time}`;
}

/** A short label for the statement: its first line, cut to `max` characters. */
export function statementExcerpt(sql: string, max = 60): string {
  const line = sql.trim().split("\n")[0] ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/**
 * Where an error position (relative to the text that ran) is in the editor: the text started at `offset`
 * (`RunRequest.offset`, the selection or statement start). Null when the position is outside the text.
 */
export function editorOffset(text: string, offset: number, position: ErrorPosition): number | null {
  const inText = positionOffset(text, position);
  return inText === null ? null : offset + inText;
}
