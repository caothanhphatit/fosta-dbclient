import type { ObjectRef } from "./contract";
import type { CellValue, TableTarget } from "./types";

export interface Navigator {
  openTable(target: TableTarget, filter?: string): void;
  /**
   * Opens the data view of an object with its column filter boxes set (column → filter text such as `=42` or `null`):
   * foreign-key navigation from the record panel and the grid's "Open related" items.
   */
  openData(connectionId: string, object: ObjectRef, columnFilters: Record<string, string>): void;
  /** True when the connection's safety rules block every write (a Read-only workspace): grids then offer no editing. */
  writesBlocked(connectionId: string): boolean;
  openVectorSearch(target: TableTarget, column: string, seed?: CellValue[]): void;
  openVectorHealth(target: TableTarget, column: string): void;
  configureEmbedding(): void;
}
