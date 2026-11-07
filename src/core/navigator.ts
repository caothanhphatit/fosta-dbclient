import type { CellValue, TableTarget } from "./types";

export interface Navigator {
  openTable(target: TableTarget, filter?: string): void;
  openVectorSearch(target: TableTarget, column: string, seed?: CellValue[]): void;
  openVectorHealth(target: TableTarget, column: string): void;
  configureEmbedding(): void;
}
