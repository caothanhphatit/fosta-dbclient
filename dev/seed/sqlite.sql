-- SQLite sample data for the engine live tests. The tests build dev/seed/shop.sqlite from this script when the file is missing
-- (the .sqlite file is generated, never committed). By hand: sqlite3 dev/seed/shop.sqlite < dev/seed/sqlite.sql

PRAGMA foreign_keys = ON;

CREATE TABLE categories (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT NOT NULL UNIQUE,
    parent_id INTEGER REFERENCES categories (id)
);

CREATE TABLE customers (
    id         INTEGER PRIMARY KEY,
    email      VARCHAR(200) NOT NULL UNIQUE,
    full_name  TEXT NOT NULL,
    country    CHAR(2) NOT NULL DEFAULT 'VN',
    created_at DATETIME NOT NULL,
    vip        BOOLEAN NOT NULL DEFAULT 0
);
CREATE INDEX customers_country_idx ON customers (country, created_at DESC);

CREATE TABLE products (
    id          INTEGER PRIMARY KEY,
    category_id INTEGER NOT NULL REFERENCES categories (id),
    sku         TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    price       NUMERIC(12, 2) NOT NULL CHECK (price >= 0),
    attributes  JSON,
    price_with_vat REAL GENERATED ALWAYS AS (round(price * 1.1, 2)) VIRTUAL
);
CREATE INDEX products_name_lower_idx ON products (lower(name));

CREATE TABLE orders (
    id          INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'new' CHECK (status IN ('new', 'paid', 'shipped', 'cancelled')),
    ordered_at  TIMESTAMP NOT NULL,
    note        TEXT
);
CREATE INDEX orders_customer_idx ON orders (customer_id);

CREATE TABLE order_items (
    order_id   INTEGER NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    line_no    INTEGER NOT NULL,
    product_id INTEGER NOT NULL REFERENCES products (id),
    quantity   INTEGER NOT NULL CHECK (quantity > 0),
    unit_price DECIMAL(12, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
) WITHOUT ROWID;

CREATE TABLE audit_log (
    happened_at TEXT NOT NULL DEFAULT (datetime('now')),
    message     TEXT NOT NULL
);

CREATE TRIGGER orders_audit AFTER UPDATE OF status ON orders
BEGIN
    INSERT INTO audit_log (message) VALUES ('order ' || NEW.id || ' is now ' || NEW.status);
END;

INSERT INTO categories (name, parent_id) VALUES ('Electronics', NULL), ('Books', NULL), ('Home', NULL), ('Phones', 1), ('Laptops', 1),
    ('Novels', 2), ('Kitchen', 3);

WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < 2000)
INSERT INTO customers (id, email, full_name, country, created_at, vip)
SELECT n, 'customer' || n || '@example.com', 'Customer ' || n,
       CASE n % 5 WHEN 0 THEN 'VN' WHEN 1 THEN 'US' WHEN 2 THEN 'DE' WHEN 3 THEN 'JP' ELSE 'FR' END,
       datetime('2025-01-01 00:00:00', '+' || (n * 37) || ' minutes'), n % 17 = 0
FROM g;

WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < 500)
INSERT INTO products (id, category_id, sku, name, price, attributes)
SELECT n, 1 + n % 7, 'SKU-' || substr('0000' || n, -5), 'Product ' || n, round(5 + (n * 7919) % 500 + (n % 100) / 100.0, 2),
       json_object('color', CASE n % 3 WHEN 0 THEN 'red' WHEN 1 THEN 'green' ELSE 'blue' END, 'weight', n % 40)
FROM g;

WITH RECURSIVE g(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM g WHERE n < 4000)
INSERT INTO orders (id, customer_id, status, ordered_at, note)
SELECT n, 1 + (n * 31) % 2000, CASE n % 4 WHEN 0 THEN 'new' WHEN 1 THEN 'paid' WHEN 2 THEN 'shipped' ELSE 'cancelled' END,
       datetime('2025-03-01 08:00:00', '+' || (n * 11) || ' minutes'), CASE WHEN n % 10 = 0 THEN 'gift wrap' END
FROM g;

WITH RECURSIVE o(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM o WHERE n < 4000),
               l(k) AS (SELECT 1 UNION ALL SELECT k + 1 FROM l WHERE k < 3)
INSERT INTO order_items (order_id, line_no, product_id, quantity, unit_price)
SELECT n, k, 1 + (n * 13 + k * 7) % 500, 1 + (n + k) % 4, 10 + (n * k) % 90
FROM o JOIN l ON k <= 1 + n % 3;

CREATE VIEW order_totals AS
SELECT o.id AS order_id, o.customer_id, o.status, sum(i.quantity * i.unit_price) AS total
FROM orders o JOIN order_items i ON i.order_id = o.id
GROUP BY o.id;

-- One row per interesting declared type, plus a row of NULLs.
CREATE TABLE type_zoo (
    id          INTEGER PRIMARY KEY,
    c_integer   INTEGER,
    c_bigint    BIGINT,
    c_real      REAL,
    c_double    DOUBLE PRECISION,
    c_numeric   NUMERIC,
    c_decimal   DECIMAL(20, 6),
    c_boolean   BOOLEAN,
    c_text      TEXT,
    c_varchar   VARCHAR(50),
    c_blob      BLOB,
    c_date      DATE,
    c_time      TIME,
    c_datetime  DATETIME,
    c_timestamp TIMESTAMP,
    c_json      JSON,
    c_uuid      UUID,
    c_untyped
);
INSERT INTO type_zoo VALUES (1, 2147483647, 9223372036854775807, 1.5, 2.718281828459045, 42, '12345.678901', 1,
    'text with ''quotes'' and unicode: Xin chào', 'varchar value', x'deadbeef', '2026-02-28', '13:45:30.123', '2026-02-28 13:45:30',
    '2026-02-28 13:45:30.5', '{"a": 1, "b": [true, null]}', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'anything');
INSERT INTO type_zoo (id) VALUES (2);

ANALYZE;
