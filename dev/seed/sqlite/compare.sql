-- Schema compare test data (src-tauri/src/contract/testing, suite schema_compare). The SQLite fixture attaches two files
-- as compare_a and compare_b; the suite runs this file before each direction, chunk by chunk (chunks are separated by
-- lines that read GO), so it must be safe to run again. The databases differ in the ways the suite expects (see the
-- comments in B). Most differences need a table rebuild on SQLite.
DROP VIEW IF EXISTS compare_a.v_order_totals;
DROP VIEW IF EXISTS compare_a.v_open_orders;
DROP VIEW IF EXISTS compare_a.v_only_b;
DROP TABLE IF EXISTS compare_a.order_items;
DROP TABLE IF EXISTS compare_a.audit_log;
DROP TABLE IF EXISTS compare_a.orders;
DROP TABLE IF EXISTS compare_a.tags;
DROP TABLE IF EXISTS compare_a.legacy_stuff;
DROP TABLE IF EXISTS compare_a.flyway_schema_history;
DROP TABLE IF EXISTS compare_a.customers;
GO
DROP VIEW IF EXISTS compare_b.v_order_totals;
DROP VIEW IF EXISTS compare_b.v_open_orders;
DROP VIEW IF EXISTS compare_b.v_only_b;
DROP TABLE IF EXISTS compare_b.order_items;
DROP TABLE IF EXISTS compare_b.audit_log;
DROP TABLE IF EXISTS compare_b.orders;
DROP TABLE IF EXISTS compare_b.tags;
DROP TABLE IF EXISTS compare_b.legacy_stuff;
DROP TABLE IF EXISTS compare_b.flyway_schema_history;
DROP TABLE IF EXISTS compare_b.customers;
GO
CREATE TABLE compare_a.customers (
  id INTEGER PRIMARY KEY,
  email VARCHAR(200) NOT NULL COLLATE NOCASE,
  name VARCHAR(100),
  CONSTRAINT uq_customers_email UNIQUE (email)
);
CREATE TABLE compare_a.orders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  customer_id INTEGER NOT NULL,
  status VARCHAR(20) NOT NULL DEFAULT 'new',
  total DECIMAL(12,2) NOT NULL DEFAULT 0,
  qty INTEGER NOT NULL DEFAULT 1,
  total_with_tax REAL GENERATED ALWAYS AS (total * 1.1) VIRTUAL,
  note TEXT,
  CONSTRAINT ck_orders_qty CHECK (qty > 0),
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id) ON DELETE CASCADE
);
CREATE INDEX compare_a.ix_orders_status ON orders (status);
CREATE INDEX compare_a.ix_orders_open ON orders (customer_id DESC) WHERE status <> 'done';
CREATE TRIGGER compare_a.tr_orders_note AFTER UPDATE OF status ON orders
BEGIN
  UPDATE orders SET note = 'status ' || new.status WHERE id = new.id;
END;
CREATE TABLE compare_a.order_items (
  id INTEGER PRIMARY KEY,
  order_id INTEGER NOT NULL REFERENCES orders (id),
  sku TEXT NOT NULL,
  qty INT NOT NULL DEFAULT 1,
  gift_note TEXT
);
CREATE INDEX compare_a.ix_items_sku ON order_items (lower(sku));
CREATE TABLE compare_a.tags (
  name TEXT NOT NULL PRIMARY KEY,
  color TEXT
) WITHOUT ROWID;
CREATE TABLE compare_a.audit_log (
  id INTEGER PRIMARY KEY,
  customer_id INTEGER REFERENCES customers (id),
  happened TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  what TEXT NOT NULL CHECK (length(what) > 0)
);
CREATE TABLE compare_a.flyway_schema_history (
  installed_rank INTEGER PRIMARY KEY,
  version TEXT
);
CREATE VIEW compare_a.v_order_totals AS
  SELECT o.id, o.total, c.email FROM orders o JOIN customers c ON c.id = o.customer_id;
CREATE VIEW compare_a.v_open_orders AS
  SELECT id, customer_id, total FROM orders WHERE status <> 'done';
GO
-- B: customers lists its columns in another order (ignored); orders changes types, defaults, nullability, a check, a
-- foreign key action and indexes, and lacks the generated column (all rebuilds); order_items has one column more and
-- one less and lacks an index (ALTER TABLE ADD/DROP COLUMN); tags is a rowid table (rebuild); legacy_stuff and
-- v_only_b exist only here; v_open_orders differs; v_order_totals is the same but reads the rebuilt orders.
CREATE TABLE compare_b.customers (
  id INTEGER PRIMARY KEY,
  name VARCHAR(100),
  email VARCHAR(200) NOT NULL COLLATE NOCASE,
  CONSTRAINT uq_customers_email UNIQUE (email)
);
CREATE TABLE compare_b.orders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  customer_id INTEGER NOT NULL,
  status VARCHAR(10) DEFAULT 'open',
  total INTEGER NOT NULL DEFAULT 0,
  qty INTEGER NOT NULL DEFAULT 1,
  note TEXT,
  legacy_code TEXT,
  CONSTRAINT ck_orders_qty CHECK (qty >= 0),
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id)
);
CREATE INDEX compare_b.ix_orders_legacy ON orders (legacy_code);
CREATE INDEX compare_b.ix_orders_open ON orders (customer_id DESC) WHERE status <> 'done';
CREATE TRIGGER compare_b.tr_orders_note AFTER UPDATE OF status ON orders
BEGIN
  UPDATE orders SET note = 'status ' || new.status WHERE id = new.id;
END;
CREATE TABLE compare_b.order_items (
  id INTEGER PRIMARY KEY,
  order_id INTEGER NOT NULL REFERENCES orders (id),
  sku TEXT NOT NULL,
  qty INT NOT NULL DEFAULT 1,
  obsolete TEXT
);
CREATE TABLE compare_b.tags (
  name TEXT NOT NULL PRIMARY KEY,
  color TEXT
);
CREATE TABLE compare_b.legacy_stuff (
  id INTEGER PRIMARY KEY,
  payload BLOB
);
CREATE VIEW compare_b.v_order_totals AS
  SELECT o.id, o.total, c.email FROM orders o JOIN customers c ON c.id = o.customer_id;
CREATE VIEW compare_b.v_open_orders AS
  SELECT id, total FROM orders WHERE status = 'open';
CREATE VIEW compare_b.v_only_b AS
  SELECT name FROM tags;
GO
-- Rows on both sides, so the rebuilds copy data and the foreign key check sees real references.
INSERT INTO compare_a.customers (id, email, name) VALUES (1, 'a@example.com', 'Ann'), (2, 'b@example.com', 'Bob');
INSERT INTO compare_a.orders (id, customer_id, status, total, qty) VALUES (1, 1, 'new', 10.5, 2), (2, 2, 'done', 3, 1);
INSERT INTO compare_a.order_items (id, order_id, sku, qty) VALUES (1, 1, 'SKU-1', 2), (2, 2, 'SKU-2', 1);
INSERT INTO compare_a.tags (name, color) VALUES ('red', '#f00');
INSERT INTO compare_b.customers (id, email, name) VALUES (1, 'a@example.com', 'Ann'), (2, 'b@example.com', 'Bob');
INSERT INTO compare_b.orders (id, customer_id, status, total, qty) VALUES (1, 1, 'open', 10, 2), (2, 2, 'done', 3, 1);
INSERT INTO compare_b.order_items (id, order_id, sku, qty) VALUES (1, 1, 'SKU-1', 2), (2, 2, 'SKU-2', 1);
INSERT INTO compare_b.tags (name, color) VALUES ('red', '#f00');
