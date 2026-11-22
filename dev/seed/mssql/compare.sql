-- Schema compare test data (src-tauri/src/contract/testing, suite schema_compare). The suite runs this file before
-- each direction, chunk by chunk (chunks are separated by lines that read GO), so it must be safe to run again.
-- Schemas compare_a and compare_b of fosta_contract differ in the ways the suite expects (see the comments in B).
IF DB_ID('fosta_contract') IS NULL CREATE DATABASE fosta_contract;
GO
USE fosta_contract;
GO
DROP VIEW IF EXISTS compare_a.v_customer_orders, compare_a.v_big_orders, compare_b.v_customer_orders, compare_b.v_big_orders;
GO
DROP TABLE IF EXISTS compare_a.items, compare_b.items;
GO
DROP TABLE IF EXISTS compare_a.orders, compare_b.orders;
GO
DROP TABLE IF EXISTS compare_a.customers, compare_b.customers, compare_a.tags, compare_b.tags, compare_a.a_only,
  compare_b.a_only, compare_a.b_only, compare_b.b_only, compare_a.flyway_schema_history, compare_b.flyway_schema_history;
GO
IF SCHEMA_ID('compare_a') IS NULL EXEC('CREATE SCHEMA compare_a');
GO
IF SCHEMA_ID('compare_b') IS NULL EXEC('CREATE SCHEMA compare_b');
GO
-- A server-named primary key and status default, a named default, a unique constraint and a table comment.
CREATE TABLE compare_a.customers (
  id INT IDENTITY(1,1) NOT NULL PRIMARY KEY,
  email NVARCHAR(200) NOT NULL,
  name NVARCHAR(100) NULL,
  status NVARCHAR(20) NOT NULL DEFAULT 'active',
  created_at DATETIME2 NOT NULL CONSTRAINT df_customers_created DEFAULT SYSDATETIME(),
  CONSTRAINT customers_email_key UNIQUE (email)
);
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Customers of the shop',
  @level0type = N'SCHEMA', @level0name = N'compare_a', @level1type = N'TABLE', @level1name = N'customers';
GO
CREATE TABLE compare_a.orders (
  id BIGINT IDENTITY(1,1) NOT NULL CONSTRAINT pk_orders PRIMARY KEY,
  customer_id INT NOT NULL,
  total DECIMAL(12,2) NOT NULL CONSTRAINT df_orders_total DEFAULT 0,
  qty BIGINT NOT NULL,
  note NVARCHAR(50) NULL,
  placed_at DATETIME2(3) NOT NULL DEFAULT SYSDATETIME(),
  shipped BIT NULL,
  double_total AS (total * 2) PERSISTED,
  CONSTRAINT orders_qty_check CHECK (qty > 0),
  CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES compare_a.customers (id) ON DELETE CASCADE
);
GO
CREATE INDEX orders_customer_idx ON compare_a.orders (customer_id) INCLUDE (total);
GO
CREATE INDEX orders_placed_idx ON compare_a.orders (placed_at DESC) WHERE shipped IS NULL;
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Including tax',
  @level0type = N'SCHEMA', @level0name = N'compare_a', @level1type = N'TABLE', @level1name = N'orders',
  @level2type = N'COLUMN', @level2name = N'total';
GO
CREATE TABLE compare_a.items (
  order_id BIGINT NOT NULL,
  sku NVARCHAR(40) NOT NULL,
  price DECIMAL(10,2) NULL,
  PRIMARY KEY (order_id, sku),
  CONSTRAINT items_order_fk FOREIGN KEY (order_id) REFERENCES compare_a.orders (id)
);
GO
CREATE TABLE compare_a.tags (
  id INT NOT NULL PRIMARY KEY,
  label NVARCHAR(50) NOT NULL,
  color NVARCHAR(20) NULL
);
GO
CREATE TRIGGER compare_a.tags_trim ON compare_a.tags AFTER INSERT AS
  UPDATE t SET label = LTRIM(RTRIM(t.label)) FROM compare_a.tags t JOIN inserted i ON i.id = t.id;
GO
CREATE TABLE compare_a.a_only (
  id INT NOT NULL PRIMARY KEY,
  label NVARCHAR(50) NULL
);
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Only in A',
  @level0type = N'SCHEMA', @level0name = N'compare_a', @level1type = N'TABLE', @level1name = N'a_only';
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Shown in lists',
  @level0type = N'SCHEMA', @level0name = N'compare_a', @level1type = N'TABLE', @level1name = N'a_only',
  @level2type = N'COLUMN', @level2name = N'label';
GO
CREATE TABLE compare_a.flyway_schema_history (
  installed_rank INT NOT NULL PRIMARY KEY,
  version NVARCHAR(50) NULL
);
GO
CREATE VIEW compare_a.v_customer_orders AS
  SELECT c.id, c.email, COUNT(o.id) AS orders
  FROM compare_a.customers c LEFT JOIN compare_a.orders o ON o.customer_id = c.id
  GROUP BY c.id, c.email;
GO
CREATE VIEW compare_a.v_big_orders AS SELECT id, qty FROM compare_a.orders WHERE qty > 10;
GO
-- customers: name narrower, another status default (server-named on both sides), no unique email, a column A does
-- not have with a default constraint of its own.
CREATE TABLE compare_b.customers (
  id INT IDENTITY(1,1) NOT NULL PRIMARY KEY,
  email NVARCHAR(200) NOT NULL,
  name NVARCHAR(50) NULL,
  status NVARCHAR(20) NOT NULL DEFAULT 'new',
  created_at DATETIME2 NOT NULL CONSTRAINT df_customers_created DEFAULT SYSDATETIME(),
  legacy NVARCHAR(MAX) NULL DEFAULT ''
);
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Customers of the shop',
  @level0type = N'SCHEMA', @level0name = N'compare_b', @level1type = N'TABLE', @level1name = N'customers';
GO
-- orders: integer keys (the primary key and a foreign key from items hold the column), no foreign key, another comment
-- on total, a wider note with its own index, a nullable placed_at, no shipped, another check, no filtered or covering
-- index. The computed column is the same.
CREATE TABLE compare_b.orders (
  id INT IDENTITY(1,1) NOT NULL CONSTRAINT pk_orders PRIMARY KEY,
  customer_id INT NOT NULL,
  total DECIMAL(12,2) NOT NULL CONSTRAINT df_orders_total DEFAULT 0,
  qty INT NOT NULL,
  note NVARCHAR(200) NULL,
  placed_at DATETIME2(3) NULL DEFAULT SYSDATETIME(),
  double_total AS (total * 2) PERSISTED,
  CONSTRAINT orders_qty_check CHECK (qty >= 0)
);
GO
CREATE INDEX orders_note_idx ON compare_b.orders (note);
GO
EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Net',
  @level0type = N'SCHEMA', @level0name = N'compare_b', @level1type = N'TABLE', @level1name = N'orders',
  @level2type = N'COLUMN', @level2name = N'total';
GO
-- items: its foreign key column follows the type change of orders.id; its server-named primary key holds the column.
CREATE TABLE compare_b.items (
  order_id INT NOT NULL,
  sku NVARCHAR(40) NOT NULL,
  price DECIMAL(10,2) NULL,
  PRIMARY KEY (order_id, sku),
  CONSTRAINT items_order_fk FOREIGN KEY (order_id) REFERENCES compare_b.orders (id)
);
GO
-- tags: the same columns in another order (identical with the default options).
CREATE TABLE compare_b.tags (
  id INT NOT NULL PRIMARY KEY,
  color NVARCHAR(20) NULL,
  label NVARCHAR(50) NOT NULL
);
GO
CREATE TRIGGER compare_b.tags_trim ON compare_b.tags AFTER INSERT AS
  UPDATE t SET label = LTRIM(RTRIM(t.label)) FROM compare_b.tags t JOIN inserted i ON i.id = t.id;
GO
CREATE TABLE compare_b.b_only (
  id INT NOT NULL PRIMARY KEY,
  payload NVARCHAR(MAX) NULL
);
GO
CREATE VIEW compare_b.v_customer_orders AS
  SELECT c.id, COUNT(o.id) AS orders
  FROM compare_b.customers c LEFT JOIN compare_b.orders o ON o.customer_id = c.id
  GROUP BY c.id;
GO
CREATE VIEW compare_b.v_big_orders AS SELECT id, qty FROM compare_b.orders WHERE qty > 10;
