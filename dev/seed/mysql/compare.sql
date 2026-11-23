-- Schema compare test data (src-tauri/src/contract/testing, suite schema_compare). The suite runs this file before
-- each direction, chunk by chunk (chunks are separated by lines that read GO), so it must be safe to run again.
-- Databases compare_a and compare_b differ in the ways the suite expects (see the comments in B).
DROP DATABASE IF EXISTS compare_a;
GO
DROP DATABASE IF EXISTS compare_b;
GO
CREATE DATABASE compare_a;
GO
CREATE DATABASE compare_b;
GO
CREATE TABLE compare_a.customers (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  email VARCHAR(200) NOT NULL,
  name VARCHAR(100) NULL,
  status VARCHAR(20) NOT NULL DEFAULT 'active',
  meta JSON NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY customers_email_key (email),
  KEY customers_email_lower ((LOWER(email)))
) COMMENT = 'Customers of the shop';
GO
CREATE TABLE compare_a.orders (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  customer_id INT NOT NULL,
  total DECIMAL(12,2) NOT NULL DEFAULT 0 COMMENT 'Including tax',
  qty BIGINT NOT NULL,
  note VARCHAR(50) NULL,
  placed_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
  shipped TINYINT(1) NULL,
  KEY orders_customer_idx (customer_id),
  KEY orders_placed_idx (placed_at DESC),
  CONSTRAINT orders_qty_check CHECK (qty > 0),
  CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES compare_a.customers (id) ON DELETE CASCADE
);
GO
CREATE TABLE compare_a.items (
  order_id BIGINT NOT NULL,
  sku VARCHAR(40) NOT NULL,
  price DECIMAL(10,2) NULL,
  PRIMARY KEY (order_id, sku),
  CONSTRAINT items_order_fk FOREIGN KEY (order_id) REFERENCES compare_a.orders (id)
);
GO
CREATE TABLE compare_a.tags (
  id INT NOT NULL PRIMARY KEY,
  label VARCHAR(50) NOT NULL,
  color VARCHAR(20) NULL
);
GO
CREATE TRIGGER compare_a.tags_trim BEFORE INSERT ON compare_a.tags FOR EACH ROW SET NEW.label = TRIM(NEW.label);
GO
CREATE TABLE compare_a.a_only (
  id INT NOT NULL PRIMARY KEY,
  label VARCHAR(50) NULL COMMENT 'Shown in lists'
) COMMENT = 'Only in A';
GO
CREATE TABLE compare_a.flyway_schema_history (
  installed_rank INT NOT NULL PRIMARY KEY,
  version VARCHAR(50) NULL
);
GO
CREATE VIEW compare_a.v_customer_orders AS
  SELECT c.id, c.email, COUNT(o.id) AS orders
  FROM compare_a.customers c LEFT JOIN compare_a.orders o ON o.customer_id = c.id
  GROUP BY c.id, c.email;
GO
CREATE VIEW compare_a.v_big_orders AS SELECT id, qty FROM compare_a.orders WHERE qty > 10;
GO
-- customers: name narrower, another status default, no unique email, no functional index, a column A does not have.
CREATE TABLE compare_b.customers (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  email VARCHAR(200) NOT NULL,
  name VARCHAR(50) NULL,
  status VARCHAR(20) NOT NULL DEFAULT 'new',
  meta JSON NULL,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  legacy TEXT NULL
) COMMENT = 'Customers of the shop';
GO
-- orders: integer keys, no foreign key, another comment on total, a wider note, a nullable placed_at, no shipped,
-- another check, other indexes.
CREATE TABLE compare_b.orders (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  customer_id INT NOT NULL,
  total DECIMAL(12,2) NOT NULL DEFAULT 0 COMMENT 'Net',
  qty INT NOT NULL,
  note VARCHAR(200) NULL,
  placed_at DATETIME NULL DEFAULT CURRENT_TIMESTAMP,
  KEY orders_note_idx (note),
  CONSTRAINT orders_qty_check CHECK (qty >= 0)
);
GO
-- items: its foreign key column follows the type change of orders.id.
CREATE TABLE compare_b.items (
  order_id INT NOT NULL,
  sku VARCHAR(40) NOT NULL,
  price DECIMAL(10,2) NULL,
  PRIMARY KEY (order_id, sku),
  CONSTRAINT items_order_fk FOREIGN KEY (order_id) REFERENCES compare_b.orders (id)
);
GO
-- tags: the same columns in another order (identical with the default options).
CREATE TABLE compare_b.tags (
  id INT NOT NULL PRIMARY KEY,
  color VARCHAR(20) NULL,
  label VARCHAR(50) NOT NULL
);
GO
CREATE TRIGGER compare_b.tags_trim BEFORE INSERT ON compare_b.tags FOR EACH ROW SET NEW.label = TRIM(NEW.label);
GO
CREATE TABLE compare_b.b_only (
  id INT NOT NULL PRIMARY KEY,
  payload JSON NULL
);
GO
CREATE VIEW compare_b.v_customer_orders AS
  SELECT c.id, COUNT(o.id) AS orders
  FROM compare_b.customers c LEFT JOIN compare_b.orders o ON o.customer_id = c.id
  GROUP BY c.id;
GO
CREATE VIEW compare_b.v_big_orders AS SELECT id, qty FROM compare_b.orders WHERE qty > 10;
