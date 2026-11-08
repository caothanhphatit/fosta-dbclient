-- ClickHouse sample data for the engine live tests (dev/compose/{postgres,clickhouse,oracle}.yml). Runs once on an empty volume.

CREATE DATABASE IF NOT EXISTS shop;

CREATE TABLE shop.categories
(
    id UInt32,
    name String,
    parent_id Nullable(UInt32)
)
ENGINE = MergeTree ORDER BY id
COMMENT 'Product categories';

INSERT INTO shop.categories VALUES (1, 'Electronics', NULL), (2, 'Books', NULL), (3, 'Home', NULL), (4, 'Phones', 1), (5, 'Laptops', 1),
    (6, 'Novels', 2), (7, 'Kitchen', 3);

CREATE TABLE shop.customers
(
    id UInt64 COMMENT 'Customer number',
    email String,
    full_name String,
    country LowCardinality(String) DEFAULT 'VN',
    created_at DateTime('UTC'),
    vip Bool DEFAULT false
)
ENGINE = MergeTree ORDER BY id;

INSERT INTO shop.customers SELECT number + 1, concat('customer', toString(number + 1), '@example.com'), concat('Customer ', toString(number + 1)),
    ['VN', 'US', 'DE', 'JP', 'FR'][1 + (number + 1) % 5], toDateTime('2025-01-01 00:00:00', 'UTC') + (number + 1) * 37 * 60,
    (number + 1) % 17 = 0
FROM numbers(2000);

CREATE TABLE shop.products
(
    id UInt32,
    category_id UInt32,
    sku String,
    name String,
    price Decimal(12, 2),
    tags Array(String),
    attributes Map(String, String)
)
ENGINE = ReplacingMergeTree ORDER BY id;

INSERT INTO shop.products SELECT number + 1, 1 + (number + 1) % 7, concat('SKU-', leftPad(toString(number + 1), 5, '0')),
    concat('Product ', toString(number + 1)), toDecimal64(5 + ((number + 1) * 7919) % 500 + ((number + 1) % 100) / 100, 2),
    [concat('tag', toString((number + 1) % 3)), concat('tag', toString((number + 1) % 5))],
    map('color', ['red', 'green', 'blue'][1 + (number + 1) % 3])
FROM numbers(500);

CREATE TABLE shop.orders
(
    id UInt64,
    customer_id UInt64,
    status Enum8('new' = 1, 'paid' = 2, 'shipped' = 3, 'cancelled' = 4),
    ordered_at DateTime64(3, 'UTC'),
    note Nullable(String)
)
ENGINE = MergeTree PARTITION BY toYYYYMM(ordered_at) ORDER BY (customer_id, id);

INSERT INTO shop.orders SELECT number + 1, 1 + ((number + 1) * 31) % 2000, 1 + (number + 1) % 4,
    toDateTime64('2025-03-01 08:00:00', 3, 'UTC') + (number + 1) * 11 * 60, if((number + 1) % 10 = 0, 'gift wrap', NULL)
FROM numbers(4000);

CREATE TABLE shop.order_items
(
    order_id UInt64,
    line_no UInt16,
    product_id UInt32,
    quantity UInt32,
    unit_price Decimal(12, 2)
)
ENGINE = MergeTree ORDER BY (order_id, line_no);

INSERT INTO shop.order_items SELECT o, l, 1 + (o * 13 + l * 7) % 500, 1 + (o + l) % 4, 10 + (o * l) % 90
FROM (SELECT number + 1 AS o FROM numbers(4000)) ARRAY JOIN range(1, 2 + o % 3) AS l;

CREATE VIEW shop.order_totals AS
SELECT o.id AS order_id, o.customer_id, o.status, sum(i.quantity * i.unit_price) AS total
FROM shop.orders AS o INNER JOIN shop.order_items AS i ON i.order_id = o.id
GROUP BY o.id, o.customer_id, o.status;

CREATE TABLE shop.daily_revenue (day Date, revenue Decimal(18, 2)) ENGINE = SummingMergeTree ORDER BY day;

CREATE MATERIALIZED VIEW shop.daily_revenue_mv TO shop.daily_revenue AS
SELECT toDate(o.ordered_at) AS day, toDecimal128(sum(i.quantity * i.unit_price), 2) AS revenue
FROM shop.order_items AS i INNER JOIN shop.orders AS o ON o.id = i.order_id
GROUP BY day;

CREATE DICTIONARY shop.category_dict (id UInt64, name String)
PRIMARY KEY id
SOURCE(CLICKHOUSE(TABLE 'categories' DB 'shop' USER 'studio' PASSWORD 'studio-root'))
LIFETIME(MIN 0 MAX 3600)
LAYOUT(FLAT());

CREATE TABLE shop.type_zoo
(
    id UInt32,
    c_bool Nullable(Bool),
    c_int8 Nullable(Int8),
    c_int16 Nullable(Int16),
    c_int32 Nullable(Int32),
    c_int64 Nullable(Int64),
    c_int128 Nullable(Int128),
    c_uint8 Nullable(UInt8),
    c_uint64 Nullable(UInt64),
    c_uint256 Nullable(UInt256),
    c_float32 Nullable(Float32),
    c_float64 Nullable(Float64),
    c_decimal Nullable(Decimal(20, 6)),
    c_string Nullable(String),
    c_fixed Nullable(FixedString(4)),
    c_lowcard LowCardinality(Nullable(String)),
    c_date Nullable(Date),
    c_date32 Nullable(Date32),
    c_datetime Nullable(DateTime('UTC')),
    c_datetime64 Nullable(DateTime64(6, 'Asia/Ho_Chi_Minh')),
    c_uuid Nullable(UUID),
    c_ipv4 Nullable(IPv4),
    c_ipv6 Nullable(IPv6),
    c_enum Nullable(Enum8('a' = 1, 'b' = 2)),
    c_array Array(Int32),
    c_array_str Array(Nullable(String)),
    c_tuple Tuple(Int32, String),
    c_map Map(String, UInt16),
    c_json Nullable(String)
)
ENGINE = MergeTree ORDER BY id;

INSERT INTO shop.type_zoo VALUES (1, true, -128, -32768, 2147483647, 9223372036854775807, -170141183460469231731687303715884105728, 255,
    18446744073709551615, 115792089237316195423570985008687907853269984665640564039457584007913129639935, 1.5, 2.718281828459045,
    12345678901234.123456, 'text with ''quotes'' and unicode: Xin chào', 'ab', 'low', '2026-02-28', '1925-06-01', '2026-02-28 13:45:30',
    '2026-02-28 13:45:30.123456', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '192.168.1.10', '2001:db8::1', 'b', [1, 2, 3], ['x', NULL],
    (7, 'seven'), {'k1': 1, 'k2': 2}, '{"a": 1}');
INSERT INTO shop.type_zoo (id) VALUES (2);

-- Big enough to stream and cancel: 1,000,000 rows.
CREATE TABLE shop.events (id UInt64, kind LowCardinality(String), at DateTime) ENGINE = MergeTree ORDER BY id;
INSERT INTO shop.events SELECT number, ['view', 'click', 'buy'][1 + number % 3], toDateTime('2025-01-01 00:00:00') + number FROM numbers(1000000);
