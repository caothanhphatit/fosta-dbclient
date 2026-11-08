# PostgreSQL

Code: `src-tauri/src/engines/postgres/` (backend), `src/engines/postgres/` (UI), `src/dev/mock/engines/postgres.ts` (dev mock).
The driver is `tokio-postgres` with rustls (`net::tls::rustls_client_config`); SSH tunnels come from the connection manager.

## What it does

- **Tree**: databases of the server, then schemas, then folders (tables, views, materialized views, foreign tables, functions,
  procedures, sequences, types) with an `Extensions` folder per database. Empty folders are left out, objects owned by extensions
  are hidden, system schemas (`pg_*`, `information_schema`) are not listed. A PostgreSQL connection is bound to one database, so
  the adapter opens one connection per database the first time you expand it.
- **Rows**: the shared grid with structured filters, raw `WHERE` filters, keyset paging, exact counts on request and the planner's
  estimate for large tables. Values are typed: `uuid`, `numeric` (exact text), `timestamptz`, `jsonb`/`json`, arrays (`text[]`
  and so on), `bytea`, enums as text; types without a neutral kind (ranges, geometry, network addresses) show their text form.
- **Edits** use optimistic concurrency. Every row of a table page carries its `xmin` (`Page.row_versions`); the grid sends it back
  and an update or delete runs as `WHERE <key> AND xmin::text = $n`. A change that matches no row rolls the whole save back and
  returns `CONFLICT` with the row as it is now. Views, foreign tables and partitioned parents have no usable `xmin`; their edits
  compare the original values (`IS NOT DISTINCT FROM`, `json` and `xml` as text). Overwrite sends the key only.
- **Query tabs**: one connection per tab, so a transaction spans runs. Plain `SELECT`/`WITH`/`VALUES` run through a server-side
  cursor and stop at the row limit without computing the rest. Stop sends PostgreSQL's CancelRequest on a second connection.
  The rules' statement timeout is `statement_timeout`, read-only is `default_transaction_read_only`. Errors carry the SQLSTATE,
  `DETAIL`, `HINT`, and the position in the script.
- **Explain**: `invoke("postgres.explain", { sql, analyze? })` returns `EXPLAIN (FORMAT JSON)` as `{ plan, analyze, sql }`.
  Without `analyze` the gateway treats it as a read. With `analyze` the statement runs (in a transaction that is rolled back), so the
  gateway classifies it as the statement inside it (`EXPLAIN ANALYZE DELETE …` is a write). The statement goes through the
  extended protocol, so a second statement in the text is refused by the server.
- **Relations**: foreign keys in both directions in the design view, and the ER diagram of a database over all user schemas
  (tables of other schemas appear as `schema.table`).
- **Object actions** (tree menu): design, copy name, explain a select, analyze, truncate (danger), refresh a materialized view,
  show a sequence's state, drop an extension (danger). They are query templates, so the gateway applies the safety rules.
- **Definition view** for functions, sequences, types and extensions: comment, facts, enum labels, composite fields, DDL.

## Connection

Form: host, port (5432), user, password, database (`postgres` when empty), application name, connect timeout, TLS
(`disable`, `require`, `verify-ca`, `verify-full`; a CA file; `accept invalid certificates` for self-signed servers) and SSH.
Connection strings: `postgres://user:pass@host:5432/db?sslmode=require&application_name=etl` or libpq `key=value` pairs.
Through an SSH tunnel the TLS name check uses the name set in the TLS section (`server name`), not `127.0.0.1`.

## Not done

Client certificates are accepted by the shared TLS builder but the form does not offer them. Composite and range columns are
read as text and written through a cast. `LISTEN/NOTIFY`, `COPY` streams and logical replication are not supported.

## Running the contract tests

```sh
docker compose -f dev/compose/postgres.yml up -d --wait
cd src-tauri && cargo test contract_postgres -- --ignored --test-threads=1
```

The suites use schema `shop` of database `shop` (`dev/seed/postgres/contract.sql`, loaded by the tests); the checks on the demo
schemas use `dev/seed/postgres.sql`, which the compose file loads. Override the server with `FOSTA_CONTRACT_POSTGRES_HOST`,
`_PORT`, `_USER`, `_PASSWORD` and `_DATABASE`.
