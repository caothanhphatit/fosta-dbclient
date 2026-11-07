-- PostgreSQL sample data for the engine live tests (dev/docker-compose.engines-sql.yml). Runs once in database `shop`.
-- Deterministic: every generated value derives from generate_series, so the tests can assert exact rows.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TYPE order_status AS ENUM ('new', 'paid', 'shipped', 'cancelled');
CREATE DOMAIN positive_money AS numeric(12, 2) CHECK (VALUE >= 0);
CREATE TYPE address AS (street text, city text, zip text);

CREATE TABLE categories (
    id        integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name      text NOT NULL UNIQUE,
    parent_id integer REFERENCES categories (id)
);
COMMENT ON TABLE categories IS 'Product categories, optionally nested';

CREATE TABLE customers (
    id         bigserial PRIMARY KEY,
    email      varchar(200) NOT NULL,
    full_name  text NOT NULL,
    country    char(2) NOT NULL DEFAULT 'VN',
    created_at timestamptz NOT NULL DEFAULT now(),
    vip        boolean NOT NULL DEFAULT false,
    CONSTRAINT customers_email_key UNIQUE (email)
);
COMMENT ON COLUMN customers.email IS 'Login and contact address';
CREATE INDEX customers_country_idx ON customers (country, created_at DESC);

CREATE TABLE products (
    id          integer PRIMARY KEY,
    category_id integer NOT NULL REFERENCES categories (id),
    sku         text NOT NULL UNIQUE,
    name        text NOT NULL,
    price       positive_money NOT NULL,
    tags        text[] NOT NULL DEFAULT '{}',
    attributes  jsonb,
    price_with_vat numeric(12, 2) GENERATED ALWAYS AS (price * 1.1) STORED
);
CREATE INDEX products_name_lower_idx ON products (lower(name));

CREATE TABLE orders (
    id          bigint PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    status      order_status NOT NULL DEFAULT 'new',
    ordered_at  timestamp NOT NULL,
    note        text
);
CREATE INDEX orders_customer_idx ON orders (customer_id);

CREATE TABLE order_items (
    order_id   bigint NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    line_no    smallint NOT NULL,
    product_id integer NOT NULL REFERENCES products (id),
    quantity   integer NOT NULL CHECK (quantity > 0),
    unit_price numeric(12, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

INSERT INTO categories (name, parent_id) VALUES ('Electronics', NULL), ('Books', NULL), ('Home', NULL);
INSERT INTO categories (name, parent_id) VALUES ('Phones', 1), ('Laptops', 1), ('Novels', 2), ('Kitchen', 3);

INSERT INTO customers (email, full_name, country, created_at, vip)
SELECT 'customer' || g || '@example.com',
       'Customer ' || g,
       (ARRAY['VN', 'US', 'DE', 'JP', 'FR'])[1 + g % 5],
       timestamptz '2025-01-01 00:00:00+00' + g * interval '37 minutes',
       g % 17 = 0
FROM generate_series(1, 2000) AS g;

INSERT INTO products (id, category_id, sku, name, price, tags, attributes)
SELECT g,
       1 + g % 7,
       'SKU-' || lpad(g::text, 5, '0'),
       'Product ' || g,
       round((5 + (g * 7919) % 500 + (g % 100) / 100.0)::numeric, 2),
       ARRAY['tag' || g % 3, 'tag' || g % 5],
       jsonb_build_object('color', (ARRAY['red', 'green', 'blue'])[1 + g % 3], 'weight', g % 40)
FROM generate_series(1, 500) AS g;

INSERT INTO orders (id, customer_id, status, ordered_at, note)
SELECT g,
       1 + (g * 31) % 2000,
       (ARRAY['new', 'paid', 'shipped', 'cancelled'])[1 + g % 4]::order_status,
       timestamp '2025-03-01 08:00:00' + g * interval '11 minutes',
       CASE WHEN g % 10 = 0 THEN 'gift wrap' END
FROM generate_series(1, 4000) AS g;

INSERT INTO order_items (order_id, line_no, product_id, quantity, unit_price)
SELECT o, l, 1 + (o * 13 + l * 7) % 500, 1 + (o + l) % 4, 10 + (o * l) % 90
FROM generate_series(1, 4000) AS o, generate_series(1, 1 + o % 3) AS l;

CREATE VIEW order_totals AS
SELECT o.id AS order_id, o.customer_id, o.status, sum(i.quantity * i.unit_price) AS total
FROM orders o JOIN order_items i ON i.order_id = o.id
GROUP BY o.id;

CREATE MATERIALIZED VIEW sales_by_category AS
SELECT c.name AS category, sum(i.quantity * i.unit_price) AS revenue
FROM order_items i JOIN products p ON p.id = i.product_id JOIN categories c ON c.id = p.category_id
GROUP BY c.name;

CREATE SEQUENCE invoice_no START 1000 INCREMENT 10;

CREATE FUNCTION customer_order_count(cid bigint) RETURNS bigint
LANGUAGE sql STABLE AS $$ SELECT count(*) FROM orders WHERE customer_id = cid $$;

CREATE SCHEMA sales;
CREATE TABLE sales.targets (
    year    integer NOT NULL,
    region  text NOT NULL,
    amount  numeric(14, 2) NOT NULL,
    PRIMARY KEY (year, region)
);
INSERT INTO sales.targets VALUES (2025, 'APAC', 1200000), (2025, 'EMEA', 950000), (2026, 'APAC', 1500000);

-- One row per interesting type, plus a row of NULLs.
CREATE TABLE type_zoo (
    id           integer PRIMARY KEY,
    c_bool       boolean,
    c_int2       smallint,
    c_int4       integer,
    c_int8       bigint,
    c_float4     real,
    c_float8     double precision,
    c_numeric    numeric(20, 6),
    c_money      money,
    c_char       char(4),
    c_varchar    varchar(50),
    c_text       text,
    c_bytea      bytea,
    c_date       date,
    c_time       time,
    c_timetz     timetz,
    c_timestamp  timestamp,
    c_timestamptz timestamptz,
    c_interval   interval,
    c_uuid       uuid,
    c_json       json,
    c_jsonb      jsonb,
    c_int_array  integer[],
    c_text_array text[],
    c_inet       inet,
    c_cidr       cidr,
    c_macaddr    macaddr,
    c_bit        bit(8),
    c_varbit     bit varying(16),
    c_point      point,
    c_range      int4range,
    c_tsrange    tsrange,
    c_enum       order_status,
    c_domain     positive_money,
    c_composite  address,
    c_xml        xml,
    c_tsvector   tsvector,
    c_oid        oid
);
INSERT INTO type_zoo VALUES (
    1, true, -32768, 2147483647, 9223372036854775807, 1.5, 2.718281828459045, 12345678901234.123456, 19.99,
    'ab', 'varchar value', 'text with ''quotes'' and unicode: Xin chào', '\xdeadbeef',
    '2026-02-28', '13:45:30.123456', '13:45:30+07', '2026-02-28 13:45:30.5', '2026-02-28 13:45:30.5+07',
    '1 year 2 mons 3 days 04:05:06', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{"a": 1, "b": [true, null]}',
    '{"nested": {"x": 1.5}}', '{1,2,3}', '{"x","y z",NULL}', '192.168.1.10/32', '10.0.0.0/8', '08:00:2b:01:02:03',
    B'10101010', B'101', '(1.5,-2)', '[1,10)', '["2026-01-01 00:00:00","2026-12-31 00:00:00")', 'paid', 42.50,
    ROW('1 Main St', 'Hanoi', '100000'), '<a>1</a>', to_tsvector('simple', 'fosta studio'), 1259
);
INSERT INTO type_zoo (id) VALUES (2);

ANALYZE;
