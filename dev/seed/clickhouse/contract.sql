-- Contract-test data (src-tauri/src/contract/testing). The tests run this file before each suite, chunk by chunk
-- (chunks are separated by lines that read GO), so it must be safe to run again. ClickHouse has no foreign keys and its
-- primary key is not unique: the unique name is only a convention of the data, and the CHECK constraint is what the
-- suites use for a rejected insert.
CREATE DATABASE IF NOT EXISTS fosta_contract
GO
DROP TABLE IF EXISTS fosta_contract.contract_children SYNC
GO
DROP TABLE IF EXISTS fosta_contract.contract_items SYNC
GO
CREATE TABLE fosta_contract.contract_items
(
    id Int32,
    name String,
    price Decimal(12, 2),
    active Bool,
    born Nullable(Date),
    seen Nullable(DateTime64(3, 'UTC')),
    tags Array(String),
    note Nullable(String),
    CONSTRAINT price_nonneg CHECK price >= 0
)
ENGINE = MergeTree
ORDER BY id
GO
INSERT INTO fosta_contract.contract_items
SELECT toInt32(number + 1),
       concat('item ', leftPad(toString(number + 1), 4, '0')),
       toDecimal64((number + 1) * 1.25, 2),
       (number + 1) % 2 = 0,
       toDate('2020-01-01') + number + 1,
       toDateTime64('2026-01-01 08:00:00.250', 3, 'UTC') + (number + 1) * 60,
       ['a', 'b'],
       if((number + 1) % 3 = 0, NULL, concat('note ', toString(number + 1)))
FROM numbers(1000)
GO
CREATE TABLE fosta_contract.contract_children
(
    id Int32,
    item_id Int32,
    label String
)
ENGINE = MergeTree
ORDER BY id
GO
INSERT INTO fosta_contract.contract_children
SELECT toInt32(number + 1), toInt32(number % 1000 + 1), concat('child ', toString(number + 1)) FROM numbers(200)
GO
DROP TABLE IF EXISTS fosta_contract.contract_vectors SYNC
GO
CREATE TABLE fosta_contract.contract_vectors
(
    id Int32,
    label String,
    embedding Array(Float32),
    INDEX embedding_idx embedding TYPE vector_similarity('hnsw', 'cosineDistance', 4),
    CONSTRAINT embedding_dims CHECK length(embedding) = 4
)
ENGINE = MergeTree
ORDER BY id
GO
INSERT INTO fosta_contract.contract_vectors
SELECT toInt32(number + 1),
       concat('vector ', toString(number + 1)),
       [toFloat32(((number + 1) % 10 + 1) / 10), toFloat32(((number + 1) * 3 % 11 + 1) / 11),
        toFloat32(((number + 1) * 7 % 13 + 1) / 13), toFloat32(((number + 1) * 9 % 17 + 1) / 17)]
FROM numbers(300)
