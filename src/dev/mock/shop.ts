/**
 * The sample "shop" database of the MariaDB and MySQL mocks (each gets its own copy, so edits stay per engine):
 * categories, customers, products (with a JSON column and a vector), orders, order items and a view.
 */
import type { ForeignKeyInfo, Value } from "../../core/contract";
import { column, v, type DatabaseSpec, type TableSpec } from "./catalog";

export const VECTOR_DIMS = 4;

const CATEGORIES = ["Desks", "Chairs", "Lighting", "Sofas", "Tables", "Storage", "Rugs", "Shelving"];
const PRODUCT_NAMES = ["Walnut desk", "Oak chair", "Steel lamp", "Linen sofa", "Glass table", "Brass hook", "Wool rug", "Pine shelf"];
const COLORS = ["walnut", "oak", "steel", "linen", "glass", "brass", "wool", "pine"];
const FIRST = ["Ava", "Liam", "Mia", "Noah", "Emma", "Lucas", "Sofia", "Ethan", "Chloe", "Mason", "Grace", "Leo"];
const LAST = ["Walker", "Bennett", "Hughes", "Carter", "Price", "Foster", "Reed", "Murphy", "Bailey", "Cooper"];
const CITIES = ["Seattle", "Austin", "Denver", "Boston", "Portland", "Chicago", "Atlanta", "San Diego"];
const STATUSES = ["pending", "paid", "shipped", "cancelled"];
const TIERS = ["bronze", "silver", "gold"];

const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const day = (n: number) => `2026-${pad((n % 10) + 1)}-${pad((n % 28) + 1)}`;
const time = (n: number) => `${pad(8 + (n % 10))}:${pad(n % 60)}:${pad((n * 7) % 60)}`;

function fk(name: string, columns: string[], refTable: string, refColumns = ["id"]): ForeignKeyInfo {
  return { name, columns, refDatabase: null, refSchema: null, refTable, refColumns, onUpdate: "RESTRICT", onDelete: "RESTRICT" };
}

function attributes(id: number): Value {
  if (id % 11 === 0) return v.null();
  return v.json({
    color: COLORS[id % COLORS.length],
    dimensions: { width: 40 + (id % 9) * 10, depth: 30 + (id % 5) * 10, height: 45 + (id % 7) * 5, unit: "cm" },
    tags: ["furniture", id % 2 ? "indoor" : "outdoor", id % 3 ? "in-stock" : "backorder"],
    care: "Wipe with a dry cloth. Avoid direct sunlight and keep away from heat sources to prevent the finish from fading.",
  });
}

function vector(id: number): Value {
  return v.other("vector", `[${(id % 10) / 10},${((id * 3) % 10) / 10},${((id * 7) % 10) / 10},${((id * 9) % 10) / 10}]`);
}

export function shopDatabase(name: string, flavor: "MariaDB" | "MySQL", counts = { products: 1240, customers: 2000, orders: 4000, items: 9000 }): DatabaseSpec {
  const id = (primary = false) => column("id", "int(11)", "int", { primary, identity: primary ? "AUTO_INCREMENT" : null, nullable: false });
  const jsonType = flavor === "MariaDB" ? "longtext" : "json";
  const tables: TableSpec[] = [
    {
      database: name,
      name: "categories",
      kind: "table",
      key: ["id"],
      columns: [id(true), column("name", "varchar(60)", "text", { nullable: false }), column("slug", "varchar(60)", "text", { nullable: false })],
      rows: () => CATEGORIES.map((label, index) => [v.int(index + 1), v.text(label), v.text(label.toLowerCase())]),
      comment: "Product categories shown in the shop navigation",
    },
    {
      database: name,
      name: "customers",
      kind: "table",
      key: ["id"],
      columns: [
        id(true),
        column("full_name", "varchar(120)", "text", { nullable: false }),
        column("email", "varchar(160)", "text", { nullable: false }),
        column("city", "varchar(60)", "text"),
        column("tier", "enum('bronze','silver','gold')", "text", { nullable: false, default: "'bronze'" }),
        column("created_at", "datetime", "timestamp", { nullable: false }),
      ],
      rows: () =>
        Array.from({ length: counts.customers }, (_, index) => {
          const n = index + 1;
          const first = FIRST[n % FIRST.length];
          const last = LAST[(n * 7) % LAST.length];
          return [
            v.int(n),
            v.text(`${first} ${last}`),
            v.text(`${first}.${last}${n}@example.com`.toLowerCase()),
            n % 13 === 0 ? v.null() : v.text(CITIES[n % CITIES.length]),
            v.text(TIERS[n % 7 === 0 ? 2 : n % 3 === 0 ? 1 : 0]),
            v.ts(`${day(n)} ${time(n)}`),
          ];
        }),
    },
    {
      database: name,
      name: "products",
      kind: "table",
      key: ["id"],
      columns: [
        id(true),
        column("sku", "varchar(32)", "text", { nullable: false }),
        column("name", "varchar(120)", "text", { nullable: false }),
        column("price", "decimal(10,2)", "decimal", { nullable: false }),
        column("stock", "int(11)", "int"),
        column("category_id", "int(11)", "int", { nullable: false }),
        column("updated_at", "datetime", "timestamp", { nullable: false }),
        column("attributes", jsonType, "json"),
        column("embedding", `vector(${VECTOR_DIMS})`, "other", { nullable: false, hints: { binary: true, vectorDims: VECTOR_DIMS } }),
      ],
      rows: () =>
        Array.from({ length: counts.products }, (_, index) => {
          const n = index + 1;
          return [
            v.int(n),
            v.text(`SKU-${pad(n, 5)}`),
            v.text(`${PRODUCT_NAMES[n % PRODUCT_NAMES.length]} ${n}`),
            v.dec((19.5 + (n % 37) * 4.25).toFixed(2)),
            n % 9 === 0 ? v.null() : v.int((n * 7) % 120),
            v.int((n % CATEGORIES.length) + 1),
            v.ts(`2026-10-${pad((n % 28) + 1)} 09:${pad(n % 60)}:00`),
            attributes(n),
            vector(n),
          ];
        }),
      foreignKeys: [fk("fk_products_category", ["category_id"], "categories")],
      indexes: [
        { name: "uq_products_sku", columns: ["sku"], unique: true, primary: false, method: "BTREE", definition: null },
        { name: "idx_embedding", columns: ["embedding"], unique: false, primary: false, method: "VECTOR", definition: "M=6 DISTANCE=cosine" },
      ],
      extra: [
        ["Engine", "InnoDB"],
        ["Collation", "utf8mb4_unicode_ci"],
      ],
    },
    {
      database: name,
      name: "orders",
      kind: "table",
      key: ["id"],
      columns: [
        id(true),
        column("customer_id", "int(11)", "int", { nullable: false }),
        column("status", "enum('pending','paid','shipped','cancelled')", "text", { nullable: false }),
        column("ordered_at", "datetime", "timestamp", { nullable: false }),
        column("total", "decimal(12,2)", "decimal", { nullable: false }),
        column("note", "varchar(255)", "text"),
      ],
      rows: () =>
        Array.from({ length: counts.orders }, (_, index) => {
          const n = index + 1;
          return [
            v.int(n),
            v.int(((n * 31) % counts.customers) + 1),
            v.text(STATUSES[n % 17 === 0 ? 3 : n % 4 === 0 ? 0 : n % 3 === 0 ? 2 : 1]),
            v.ts(`${day(n)} ${time(n)}`),
            v.dec((25 + ((n * 37) % 900) + (n % 100) / 100).toFixed(2)),
            n % 6 === 0 ? v.text(n % 12 === 0 ? "Leave at the front desk" : "Gift wrap, please") : v.null(),
          ];
        }),
      foreignKeys: [fk("fk_orders_customer", ["customer_id"], "customers")],
    },
    {
      database: name,
      name: "order_items",
      kind: "table",
      key: ["id"],
      columns: [
        id(true),
        column("order_id", "int(11)", "int", { nullable: false }),
        column("product_id", "int(11)", "int", { nullable: false }),
        column("quantity", "int(11)", "int", { nullable: false }),
        column("unit_price", "decimal(10,2)", "decimal", { nullable: false }),
      ],
      rows: () =>
        Array.from({ length: counts.items }, (_, index) => {
          const n = index + 1;
          return [
            v.int(n),
            v.int(((n * 13) % counts.orders) + 1),
            v.int(((n * 17) % counts.products) + 1),
            v.int((n % 4) + 1),
            v.dec((19.5 + (n % 37) * 4.25).toFixed(2)),
          ];
        }),
      foreignKeys: [fk("fk_items_order", ["order_id"], "orders"), fk("fk_items_product", ["product_id"], "products")],
    },
    {
      database: name,
      name: "v_daily_sales",
      kind: "view",
      key: [],
      columns: [column("day", "date", "date"), column("orders", "bigint(21)", "int", { nullable: false }), column("revenue", "decimal(34,2)", "decimal")],
      rows: () =>
        Array.from({ length: 60 }, (_, index) => [
          v.date(`2026-${pad(Math.floor(index / 28) + 9)}-${pad((index % 28) + 1)}`),
          v.int(40 + ((index * 7) % 35)),
          v.dec((3200 + ((index * 811) % 2900)).toFixed(2)),
        ]),
      ddl: "CREATE VIEW `v_daily_sales` AS SELECT CAST(`ordered_at` AS DATE) AS `day`, COUNT(*) AS `orders`, SUM(`total`) AS `revenue` FROM `orders` GROUP BY 1",
    },
  ];
  return { name, tables };
}

export function analyticsDatabase(name = "analytics"): DatabaseSpec {
  const pages = ["/", "/products", "/products/42", "/cart", "/checkout", "/account", "/search?q=desk"];
  return {
    name,
    tables: [
      {
        database: name,
        name: "page_views",
        kind: "table",
        key: ["id"],
        columns: [
          column("id", "bigint(20)", "int", { primary: true, identity: "AUTO_INCREMENT", nullable: false }),
          column("session_id", "char(36)", "uuid", { nullable: false }),
          column("path", "varchar(255)", "text", { nullable: false }),
          column("duration_ms", "int(11)", "int"),
          column("bounced", "tinyint(1)", "bool", { nullable: false }),
          column("viewed_at", "timestamp", "timestamp", { nullable: false }),
        ],
        rows: () =>
          Array.from({ length: 600 }, (_, index) => {
            const n = index + 1;
            const hex = (n * 2654435761).toString(16).padStart(12, "0").slice(-12);
            return [
              v.int(n),
              v.uuid(`3f2a9c4e-${pad(n % 9999, 4)}-4b1d-9e2f-${hex}`),
              v.text(pages[n % pages.length]),
              n % 10 === 0 ? v.null() : v.int(400 + ((n * 97) % 9000)),
              v.bool(n % 5 === 0),
              v.ts(`${day(n)} ${time(n)}`),
            ];
          }),
      },
    ],
  };
}

/** The system schemas of a MySQL-family server (no tables in the mock). */
export function mysqlSystemDatabases(): DatabaseSpec[] {
  return ["information_schema", "mysql", "performance_schema", "sys"].map((name) => ({ name, system: true, tables: [] }));
}
