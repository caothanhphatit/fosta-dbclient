-- Contract-test data (src-tauri/src/contract/testing). The tests run this file before each suite, chunk by chunk
-- (chunks are separated by lines that read GO), so it must be safe to run again.
DROP TABLE IF EXISTS contract_children;
GO
DROP TABLE IF EXISTS contract_items;
GO
CREATE TABLE contract_items (
  id INTEGER NOT NULL PRIMARY KEY,
  name VARCHAR(100) NOT NULL UNIQUE,
  price DECIMAL(12,2) NOT NULL,
  active BOOLEAN NOT NULL,
  born DATE,
  seen DATETIME,
  meta JSON,
  blob_data BLOB,
  note TEXT
);
GO
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 1000)
INSERT INTO contract_items (id, name, price, active, born, seen, meta, blob_data, note)
SELECT n,
       'item ' || printf('%04d', n),
       n * 1.25,
       n % 2 = 0,
       date('2020-01-01', '+' || n || ' days'),
       strftime('%Y-%m-%d %H:%M:%f', '2026-01-01 08:00:00.250', '+' || n || ' minutes'),
       json_object('n', n, 'tags', json_array('a', 'b')),
       unhex(printf('%08x', n)),
       CASE WHEN n % 3 = 0 THEN NULL ELSE 'note ' || n END
FROM seq;
GO
CREATE TABLE contract_children (
  id INTEGER NOT NULL PRIMARY KEY,
  item_id INTEGER NOT NULL,
  label VARCHAR(50) NOT NULL,
  CONSTRAINT fk_contract_children_item FOREIGN KEY (item_id) REFERENCES contract_items (id)
);
GO
WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 200)
INSERT INTO contract_children (id, item_id, label) SELECT n, n, 'child ' || n FROM seq;
