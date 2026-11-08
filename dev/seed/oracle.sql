-- Oracle sample data for the engine live tests (dev/compose/{postgres,clickhouse,oracle}.yml). Runs once, as SYSDBA, on an empty volume;
-- the objects belong to the application user SHOP in the pluggable database FREEPDB1.

ALTER SESSION SET CONTAINER = FREEPDB1;
ALTER USER shop QUOTA UNLIMITED ON users;
GRANT CREATE SESSION, CREATE TABLE, CREATE VIEW, CREATE MATERIALIZED VIEW, CREATE SEQUENCE, CREATE PROCEDURE, CREATE SYNONYM,
      CREATE TYPE TO shop;
ALTER SESSION SET CURRENT_SCHEMA = shop;

CREATE TABLE categories (
    id        NUMBER(10) GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name      VARCHAR2(100) NOT NULL CONSTRAINT categories_name_uk UNIQUE,
    parent_id NUMBER(10) CONSTRAINT categories_parent_fk REFERENCES categories (id)
);
COMMENT ON TABLE categories IS 'Product categories';

CREATE TABLE customers (
    id         NUMBER(19) PRIMARY KEY,
    email      VARCHAR2(200) NOT NULL CONSTRAINT customers_email_uk UNIQUE,
    full_name  NVARCHAR2(200) NOT NULL,
    country    CHAR(2) DEFAULT 'VN' NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    vip        NUMBER(1) DEFAULT 0 NOT NULL
);
COMMENT ON COLUMN customers.email IS 'Login and contact address';
CREATE INDEX customers_country_idx ON customers (country, created_at DESC);

CREATE TABLE products (
    id             NUMBER(10) PRIMARY KEY,
    category_id    NUMBER(10) NOT NULL CONSTRAINT products_category_fk REFERENCES categories (id),
    sku            VARCHAR2(20) NOT NULL UNIQUE,
    name           VARCHAR2(200) NOT NULL,
    price          NUMBER(12, 2) NOT NULL CHECK (price >= 0),
    attributes     CLOB CHECK (attributes IS JSON),
    price_with_vat NUMBER(12, 2) GENERATED ALWAYS AS (ROUND(price * 1.1, 2)) VIRTUAL
);

CREATE TABLE orders (
    id          NUMBER(19) PRIMARY KEY,
    customer_id NUMBER(19) NOT NULL CONSTRAINT orders_customer_fk REFERENCES customers (id) ON DELETE CASCADE,
    status      VARCHAR2(10) DEFAULT 'new' NOT NULL CHECK (status IN ('new', 'paid', 'shipped', 'cancelled')),
    ordered_at  DATE NOT NULL,
    note        VARCHAR2(400)
);
CREATE INDEX orders_customer_idx ON orders (customer_id);

CREATE TABLE order_items (
    order_id   NUMBER(19) NOT NULL CONSTRAINT order_items_order_fk REFERENCES orders (id) ON DELETE CASCADE,
    line_no    NUMBER(5) NOT NULL,
    product_id NUMBER(10) NOT NULL CONSTRAINT order_items_product_fk REFERENCES products (id),
    quantity   NUMBER(10) NOT NULL CHECK (quantity > 0),
    unit_price NUMBER(12, 2) NOT NULL,
    CONSTRAINT order_items_pk PRIMARY KEY (order_id, line_no)
);

INSERT INTO categories (name, parent_id) VALUES ('Electronics', NULL);
INSERT INTO categories (name, parent_id) VALUES ('Books', NULL);
INSERT INTO categories (name, parent_id) VALUES ('Home', NULL);
INSERT INTO categories (name, parent_id) VALUES ('Phones', 1);
INSERT INTO categories (name, parent_id) VALUES ('Laptops', 1);
INSERT INTO categories (name, parent_id) VALUES ('Novels', 2);
INSERT INTO categories (name, parent_id) VALUES ('Kitchen', 3);

INSERT INTO customers
SELECT LEVEL, 'customer' || LEVEL || '@example.com', 'Customer ' || LEVEL,
       DECODE(MOD(LEVEL, 5), 0, 'VN', 1, 'US', 2, 'DE', 3, 'JP', 'FR'),
       TIMESTAMP '2025-01-01 00:00:00 +00:00' + NUMTODSINTERVAL(LEVEL * 37, 'MINUTE'),
       CASE WHEN MOD(LEVEL, 17) = 0 THEN 1 ELSE 0 END
FROM dual CONNECT BY LEVEL <= 2000;

INSERT INTO products (id, category_id, sku, name, price, attributes)
SELECT LEVEL, 1 + MOD(LEVEL, 7), 'SKU-' || LPAD(LEVEL, 5, '0'), 'Product ' || LEVEL,
       ROUND(5 + MOD(LEVEL * 7919, 500) + MOD(LEVEL, 100) / 100, 2),
       JSON_OBJECT('color' VALUE DECODE(MOD(LEVEL, 3), 0, 'red', 1, 'green', 'blue'), 'weight' VALUE MOD(LEVEL, 40))
FROM dual CONNECT BY LEVEL <= 500;

INSERT INTO orders
SELECT LEVEL, 1 + MOD(LEVEL * 31, 2000), DECODE(MOD(LEVEL, 4), 0, 'new', 1, 'paid', 2, 'shipped', 'cancelled'),
       DATE '2025-03-01' + 8 / 24 + LEVEL * 11 / 1440, CASE WHEN MOD(LEVEL, 10) = 0 THEN 'gift wrap' END
FROM dual CONNECT BY LEVEL <= 4000;

INSERT INTO order_items
SELECT o.n, l.k, 1 + MOD(o.n * 13 + l.k * 7, 500), 1 + MOD(o.n + l.k, 4), 10 + MOD(o.n * l.k, 90)
FROM (SELECT LEVEL AS n FROM dual CONNECT BY LEVEL <= 4000) o
JOIN (SELECT LEVEL AS k FROM dual CONNECT BY LEVEL <= 3) l ON l.k <= 1 + MOD(o.n, 3);

CREATE VIEW order_totals AS
SELECT o.id AS order_id, o.customer_id, o.status, SUM(i.quantity * i.unit_price) AS total
FROM orders o JOIN order_items i ON i.order_id = o.id
GROUP BY o.id, o.customer_id, o.status;

CREATE MATERIALIZED VIEW sales_by_category AS
SELECT c.name AS category, SUM(i.quantity * i.unit_price) AS revenue
FROM order_items i JOIN products p ON p.id = i.product_id JOIN categories c ON c.id = p.category_id
GROUP BY c.name;

CREATE SEQUENCE invoice_no START WITH 1000 INCREMENT BY 10;
CREATE SYNONYM cust FOR customers;

CREATE OR REPLACE FUNCTION customer_order_count (cid NUMBER) RETURN NUMBER IS
    n NUMBER;
BEGIN
    SELECT COUNT(*) INTO n FROM orders WHERE customer_id = cid;
    RETURN n;
END;
/

CREATE OR REPLACE PROCEDURE touch_order (oid NUMBER) IS
BEGIN
    UPDATE orders SET note = note WHERE id = oid;
END;
/

CREATE OR REPLACE PACKAGE shop_pkg AS
    FUNCTION version RETURN VARCHAR2;
END shop_pkg;
/

CREATE OR REPLACE PACKAGE BODY shop_pkg AS
    FUNCTION version RETURN VARCHAR2 IS
    BEGIN
        RETURN '1.0';
    END;
END shop_pkg;
/

-- One row per interesting type, plus a row of NULLs.
CREATE TABLE type_zoo (
    id              NUMBER(10) PRIMARY KEY,
    c_number        NUMBER,
    c_number_scale  NUMBER(20, 6),
    c_integer       NUMBER(19),
    c_binary_float  BINARY_FLOAT,
    c_binary_double BINARY_DOUBLE,
    c_float         FLOAT,
    c_varchar2      VARCHAR2(100),
    c_nvarchar2     NVARCHAR2(100),
    c_char          CHAR(4),
    c_clob          CLOB,
    c_nclob         NCLOB,
    c_blob          BLOB,
    c_raw           RAW(16),
    c_date          DATE,
    c_timestamp     TIMESTAMP(6),
    c_timestamp_tz  TIMESTAMP(6) WITH TIME ZONE,
    c_interval_ym   INTERVAL YEAR TO MONTH,
    c_interval_ds   INTERVAL DAY TO SECOND,
    c_boolean       BOOLEAN,
    c_json          JSON
);
INSERT INTO type_zoo VALUES (
    1, 3.14159, 12345678901234.123456, 9223372036854775807, 1.5, 2.718281828459045, 0.1,
    'text with ''quotes'' and unicode: Xin chào', N'Xin chào', 'ab', 'a CLOB value', N'an NCLOB value', HEXTORAW('DEADBEEF'),
    HEXTORAW('A0EEBC999C0B4EF8BB6D6BB9BD380A11'), TO_DATE('2026-02-28 13:45:30', 'YYYY-MM-DD HH24:MI:SS'),
    TIMESTAMP '2026-02-28 13:45:30.123456', TIMESTAMP '2026-02-28 13:45:30.5 +07:00', INTERVAL '1-2' YEAR TO MONTH,
    INTERVAL '3 04:05:06.5' DAY TO SECOND, TRUE, JSON('{"a": 1, "b": [true, null]}')
);
INSERT INTO type_zoo (id) VALUES (2);
COMMIT;

BEGIN
    DBMS_STATS.GATHER_SCHEMA_STATS('SHOP');
END;
/
