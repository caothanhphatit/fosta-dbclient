// A small shop schema shared by the language tests.
import type { CompletionColumn, CompletionSchema } from "./types";

const col = (name: string, type: string, extra: Partial<CompletionColumn> = {}): CompletionColumn => ({ name, type, pk: false, ...extra });

export const SHOP: CompletionSchema = {
  databases: ["shop"],
  schemas: ["public", "sales"],
  defaultSchema: "public",
  tables: [
    { name: "customers", schema: "public", kind: "table", columns: [col("id", "int", { pk: true }), col("name", "text"), col("email", "text")] },
    {
      name: "orders",
      schema: "public",
      kind: "table",
      columns: [
        col("id", "int", { pk: true }),
        col("customer_id", "int", { fk: { table: "customers", column: "id" } }),
        col("total", "numeric"),
        col("created_at", "timestamp"),
      ],
    },
    {
      name: "order_items",
      schema: "public",
      kind: "table",
      columns: [
        col("id", "int", { pk: true }),
        col("order_id", "int", { fk: { table: "orders", column: "id" } }),
        col("product_id", "int", { fk: { table: "products", column: "id" } }),
        col("qty", "int"),
      ],
    },
    { name: "products", schema: "public", kind: "table", columns: [col("id", "int", { pk: true }), col("name", "text"), col("price", "numeric")] },
    { name: "Mixed Case", schema: "public", kind: "view", columns: [col("Value", "int")] },
    {
      name: "invoices",
      schema: "sales",
      kind: "table",
      columns: [col("id", "int", { pk: true }), col("order_id", "int", { fk: { table: "orders", column: "id", schema: "public" } })],
    },
  ],
};

/** Splits `text` at the `|` cursor marker. */
export function cursor(text: string): { doc: string; pos: number } {
  const pos = text.indexOf("|");
  if (pos < 0) throw new Error("no cursor marker");
  return { doc: text.slice(0, pos) + text.slice(pos + 1), pos };
}
