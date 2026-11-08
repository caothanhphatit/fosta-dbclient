-- Contract-test data (src-tauri/src/contract/testing). The tests run this file before each suite, chunk by chunk
-- (chunks are separated by lines that read GO), so it must be safe to run again. contract_items has a rowversion
-- column, which the adapter uses for optimistic concurrency.
IF DB_ID('fosta_contract') IS NULL CREATE DATABASE fosta_contract;
GO
IF OBJECT_ID('fosta_contract.dbo.contract_children') IS NOT NULL DROP TABLE fosta_contract.dbo.contract_children;
GO
IF OBJECT_ID('fosta_contract.dbo.contract_items') IS NOT NULL DROP TABLE fosta_contract.dbo.contract_items;
GO
CREATE TABLE fosta_contract.dbo.contract_items (
  id INT NOT NULL PRIMARY KEY,
  name NVARCHAR(100) NOT NULL CONSTRAINT uq_contract_items_name UNIQUE,
  price DECIMAL(12,2) NOT NULL,
  active BIT NOT NULL,
  born DATE NULL,
  seen DATETIME2(3) NULL,
  meta NVARCHAR(MAX) NULL,
  blob_data VARBINARY(64) NULL,
  note NVARCHAR(200) NULL,
  rv ROWVERSION
);
GO
WITH n AS (
  SELECT TOP (1000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i
  FROM sys.all_objects a CROSS JOIN sys.all_objects b
)
INSERT INTO fosta_contract.dbo.contract_items (id, name, price, active, born, seen, meta, blob_data, note)
SELECT i,
       CONCAT('item ', RIGHT('0000' + CAST(i AS VARCHAR(4)), 4)),
       i * 1.25,
       CASE WHEN i % 2 = 0 THEN 1 ELSE 0 END,
       DATEADD(DAY, i, '2020-01-01'),
       DATEADD(MINUTE, i, CAST('2026-01-01 08:00:00.250' AS DATETIME2(3))),
       CONCAT('{"n":', i, ',"tags":["a","b"]}'),
       CAST(i AS VARBINARY(4)),
       CASE WHEN i % 3 = 0 THEN NULL ELSE CONCAT('note ', i) END
FROM n;
GO
CREATE TABLE fosta_contract.dbo.contract_children (
  id INT NOT NULL PRIMARY KEY,
  item_id INT NOT NULL CONSTRAINT fk_contract_children_item REFERENCES fosta_contract.dbo.contract_items (id),
  label NVARCHAR(50) NOT NULL
);
GO
INSERT INTO fosta_contract.dbo.contract_children (id, item_id, label)
SELECT id, id, CONCAT('child ', id) FROM fosta_contract.dbo.contract_items WHERE id <= 200;
