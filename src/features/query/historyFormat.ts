/** Pure formatting for query history entries (no DOM), shared by the query tab and its history panel. */
import type { HistoryEntry } from "../../core/types";

export type HistoryStatus = "ok" | "error" | "cancelled";

const numberFormat = new Intl.NumberFormat("en-US");

export function historyStatus(entry: Pick<HistoryEntry, "error" | "cancelled">): HistoryStatus {
  if (entry.cancelled) return "cancelled";
  return entry.error ? "error" : "ok";
}

/** "3 rows", "12 affected", "2 rows · 1 affected", "1,000+ rows", the error, or "Cancelled". */
export function resultSummary(entry: Pick<HistoryEntry, "results" | "error" | "cancelled">): string {
  if (entry.cancelled) return "Cancelled";
  if (entry.error) return firstLine(entry.error);
  if (entry.results.length === 0) return "No result";
  return entry.results
    .map((result) =>
      result.rowSet
        ? `${numberFormat.format(result.rows)}${result.truncated ? "+" : ""} ${result.rows === 1 && !result.truncated ? "row" : "rows"}`
        : `${numberFormat.format(result.affectedRows)} affected`,
    )
    .join(" · ");
}

/** "now", "45 s ago", "5 min ago", "3 h ago", "2 d ago", then the date. */
export function relativeTime(timestamp: number, now: number): string {
  const seconds = Math.round((now - timestamp) / 1000);
  if (seconds < 10) return "now";
  if (seconds < 60) return `${seconds} s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} d ago`;
  return new Date(timestamp).toISOString().slice(0, 10);
}

/** "850 ms", "1.2 s", "2 min 5 s". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1000);
  return seconds ? `${minutes} min ${seconds} s` : `${minutes} min`;
}

/** The first non-empty line, trimmed and shortened for a list row. */
export function firstLine(text: string, max = 160): string {
  const line =
    text
      .split(/\r?\n/)
      .map((item) => item.trim())
      .find(Boolean) ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}
