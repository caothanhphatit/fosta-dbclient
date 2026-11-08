-- Contract-test data (src-tauri/src/contract/testing) for the SHOP schema of dev/compose/oracle.yml. The tests run this
-- file before each suite, chunk by chunk (chunks are separated by lines that read GO), so it must be safe to run again.
-- Table names are unquoted (CONTRACT_ITEMS) and column names quoted lower case ("id"), the names the shared suites use.
DROP TABLE IF EXISTS contract_children PURGE
GO
DROP TABLE IF EXISTS contract_items PURGE
GO
DROP TABLE IF EXISTS contract_loose PURGE
GO
CREATE TABLE contract_items (
  "id" NUMBER(10) NOT NULL PRIMARY KEY,
  "name" VARCHAR2(100) NOT NULL CONSTRAINT uq_contract_items_name UNIQUE,
  "price" NUMBER(12,2) NOT NULL,
  "active" BOOLEAN NOT NULL,
  "born" DATE,
  "seen" TIMESTAMP(3),
  "meta" JSON,
  "blob_data" BLOB,
  "doc" CLOB,
  "note" VARCHAR2(200)
)
GO
INSERT INTO contract_items ("id", "name", "price", "active", "born", "seen", "meta", "blob_data", "doc", "note")
SELECT LEVEL,
       'item ' || LPAD(LEVEL, 4, '0'),
       LEVEL * 1.25,
       CASE WHEN MOD(LEVEL, 2) = 0 THEN TRUE ELSE FALSE END,
       DATE '2020-01-01' + LEVEL,
       TIMESTAMP '2026-01-01 08:00:00.250' + NUMTODSINTERVAL(LEVEL, 'MINUTE'),
       JSON('{"n":' || LEVEL || ',"tags":["a","b"]}'),
       HEXTORAW(LPAD(TO_CHAR(LEVEL, 'FMXXXXXXXX'), 8, '0')),
       TO_CLOB('doc ' || LEVEL),
       CASE WHEN MOD(LEVEL, 3) = 0 THEN NULL ELSE 'note ' || LEVEL END
FROM dual CONNECT BY LEVEL <= 1000
GO
CREATE TABLE contract_children (
  "id" NUMBER(10) NOT NULL PRIMARY KEY,
  "item_id" NUMBER(10) NOT NULL,
  "label" VARCHAR2(50) NOT NULL,
  CONSTRAINT fk_contract_children_item FOREIGN KEY ("item_id") REFERENCES contract_items ("id")
)
GO
INSERT INTO contract_children ("id", "item_id", "label") SELECT LEVEL, LEVEL, 'child ' || LEVEL FROM dual CONNECT BY LEVEL <= 200
GO
-- No primary key: browsed and edited by ROWID.
CREATE TABLE contract_loose ("name" VARCHAR2(20), "qty" NUMBER, "doc" CLOB)
GO
INSERT INTO contract_loose ("name", "qty", "doc") SELECT 'loose ' || LEVEL, LEVEL, TO_CLOB('text ' || LEVEL) FROM dual CONNECT BY LEVEL <= 3
GO
COMMIT
