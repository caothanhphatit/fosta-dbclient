import type { CellValue, TableTarget } from "./types";

export interface Navigator {
  openTable(target: TableTarget, filter?: string): void;
  /** True when the connection's safety rules block every write (a Read-only workspace): grids then offer no editing. */
  writesBlocked(connectionId: string): boolean;
  openVectorSearch(target: TableTarget, column: string, seed?: CellValue[]): void;
  openVectorHealth(target: TableTarget, column: string): void;
  configureEmbedding(): void;
}
