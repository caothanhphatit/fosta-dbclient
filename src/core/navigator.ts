import type { ObjectRef } from "./contract";
import type { KeyCell, TableTarget } from "./types";

export interface Navigator {
  openTable(target: TableTarget, filter?: string): void;
  /**
   * Opens the data view of an object with its column filter boxes set (column → filter text such as `=42` or `null`):
   * foreign-key navigation from the record panel and the grid's "Open related" items.
   */
  openData(connectionId: string, object: ObjectRef, columnFilters: Record<string, string>): void;
  /**
   * Opens the engine workspace of an object (or selects its tab) on `section` and hands that section `request`
   * (e.g. a filter to run): a link from one collection's document to another collection.
   */
  openWorkspace(connectionId: string, object: ObjectRef, section: string, request: unknown): void;
  /** True when the connection's safety rules block every write (a Read-only workspace): grids then offer no editing. */
  writesBlocked(connectionId: string): boolean;
  openVectorSearch(target: TableTarget, column: string, seed?: KeyCell[]): void;
  openVectorHealth(target: TableTarget, column: string): void;
  configureEmbedding(): void;
  /**
   * Opens the assistant panel and sends `prompt` (an inline "Ask" in a view). `extra` is the context of this message
   * only (the filter, the stage and its error); `samples` are sampled documents, which reach the model only where the
   * workspace lets the assistant read data.
   */
  askAssistant?(prompt: string, extra?: string, samples?: string): void;
}
