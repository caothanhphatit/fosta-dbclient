-- Contract-test data (src-tauri/src/contract/testing). The tests run this file before each suite, chunk by chunk
-- (chunks are separated by lines that read GO), so it must be safe to run again.
CREATE DATABASE IF NOT EXISTS fosta_contract;
GO
DROP TABLE IF EXISTS fosta_contract.contract_children;
GO
DROP TABLE IF EXISTS fosta_contract.contract_items;
GO
CREATE TABLE fosta_contract.contract_items (
  id INT NOT NULL PRIMARY KEY,
  name VARCHAR(100) NOT NULL,
  price DECIMAL(12,2) NOT NULL,
  active BOOLEAN NOT NULL,
  born DATE NULL,
  seen DATETIME(3) NULL,
  meta JSON NULL,
  blob_data VARBINARY(64) NULL,
  note VARCHAR(200) NULL,
  UNIQUE KEY uq_contract_items_name (name)
);
GO
INSERT INTO fosta_contract.contract_items (id, name, price, active, born, seen, meta, blob_data, note)
SELECT seq,
       CONCAT('item ', LPAD(seq, 4, '0')),
       seq * 1.25,
       seq % 2 = 0,
       DATE_ADD('2020-01-01', INTERVAL seq DAY),
       TIMESTAMP('2026-01-01 08:00:00.250') + INTERVAL seq MINUTE,
       JSON_OBJECT('n', seq, 'tags', JSON_ARRAY('a', 'b')),
       UNHEX(LPAD(HEX(seq), 8, '0')),
       IF(seq % 3 = 0, NULL, CONCAT('note ', seq))
FROM fosta_contract.seq_1_to_1000;
GO
CREATE TABLE fosta_contract.contract_children (
  id INT NOT NULL PRIMARY KEY,
  item_id INT NOT NULL,
  label VARCHAR(50) NOT NULL,
  CONSTRAINT fk_contract_children_item FOREIGN KEY (item_id) REFERENCES fosta_contract.contract_items (id)
);
GO
INSERT INTO fosta_contract.contract_children (id, item_id, label)
SELECT seq, seq, CONCAT('child ', seq) FROM fosta_contract.seq_1_to_200;
