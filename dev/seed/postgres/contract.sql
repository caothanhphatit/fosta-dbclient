-- Contract-test data (src-tauri/src/contract/testing). The tests run this file before each suite, chunk by chunk
-- (chunks are separated by lines that read GO), so it must be safe to run again.
-- The tables live in schema `shop` of database `shop`: the suites write `<database>.<table>`, which PostgreSQL reads as
-- `<schema>.<table>`, so the schema carries the database's name.
CREATE SCHEMA IF NOT EXISTS shop;
GO
DROP TABLE IF EXISTS shop.contract_children;
GO
DROP TABLE IF EXISTS shop.contract_items;
GO
CREATE TABLE shop.contract_items (
  id integer NOT NULL PRIMARY KEY,
  name varchar(100) NOT NULL,
  price numeric(12,2) NOT NULL,
  active boolean NOT NULL,
  born date,
  seen timestamp(3),
  meta jsonb,
  blob_data bytea,
  note text,
  tags text[],
  uid uuid,
  at timestamptz,
  CONSTRAINT uq_contract_items_name UNIQUE (name)
);
GO
INSERT INTO shop.contract_items (id, name, price, active, born, seen, meta, blob_data, note, tags, uid, at)
SELECT seq,
       'item ' || lpad(seq::text, 4, '0'),
       seq * 1.25,
       seq % 2 = 0,
       DATE '2020-01-01' + seq,
       TIMESTAMP '2026-01-01 08:00:00.250' + seq * INTERVAL '1 minute',
       jsonb_build_object('n', seq, 'tags', jsonb_build_array('a', 'b')),
       decode(lpad(to_hex(seq), 8, '0'), 'hex'),
       CASE WHEN seq % 3 = 0 THEN NULL ELSE 'note ' || seq END,
       ARRAY['a', 'b' || seq],
       md5('contract' || seq)::uuid,
       TIMESTAMPTZ '2026-01-01 08:00:00+00' + seq * INTERVAL '1 minute'
FROM generate_series(1, 1000) AS seq;
GO
CREATE TABLE shop.contract_children (
  id integer NOT NULL PRIMARY KEY,
  item_id integer NOT NULL,
  label varchar(50) NOT NULL,
  CONSTRAINT fk_contract_children_item FOREIGN KEY (item_id) REFERENCES shop.contract_items (id)
);
GO
INSERT INTO shop.contract_children (id, item_id, label)
SELECT seq, seq, 'child ' || seq FROM generate_series(1, 200) AS seq;
GO
CREATE EXTENSION IF NOT EXISTS vector;
GO
DROP TABLE IF EXISTS shop.contract_vectors;
GO
CREATE TABLE shop.contract_vectors (
  id integer NOT NULL PRIMARY KEY,
  label varchar(50) NOT NULL,
  embedding vector(4) NOT NULL
);
GO
INSERT INTO shop.contract_vectors (id, label, embedding)
SELECT seq,
       'vector ' || seq,
       ('[' || ((seq % 10) + 1) / 10.0 || ',' || ((seq * 3) % 11 + 1) / 11.0 || ',' || ((seq * 7) % 13 + 1) / 13.0 || ','
            || ((seq * 9) % 17 + 1) / 17.0 || ']')::vector(4)
FROM generate_series(1, 300) AS seq;
GO
CREATE INDEX contract_vectors_embedding_idx ON shop.contract_vectors USING hnsw (embedding vector_cosine_ops) WITH (m = 8);
