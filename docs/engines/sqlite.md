# SQLite

Code: `src-tauri/src/engines/sqlite/` (backend), `src/engines/sqlite/` (UI), `src/dev/mock/engines/sqlite.ts` (dev mock).
The driver is `rusqlite` with the bundled SQLite (3.50). It is synchronous, so every call runs on Tokio's blocking pool.

## What it does

- **Open a file**: the connection form has a file picker field (`.db`, `.sqlite`, `.sqlite3`) instead of host and port; no
  password. Switches: open read-only, create the file when it is missing, busy timeout. `:memory:` opens a database that the tree,
  the grid and every query tab of the connection share (it is gone when you disconnect).
- **Attached databases** (optional): the `Attached databases` field takes one `alias=path` per line. They are attached to every
  connection of the profile, show up next to `main` in the tree and are used in queries as `alias.table`. A missing file is an
  error, not a new empty database (unless "create" is on); with "open read-only" they are attached read-only too.
- **Tree**: databases (`main`, `temp`, attached) with folders for tables, views, indexes and triggers (empty folders are left out).
  Indexes and triggers open a definition view with their stored SQL.
- **Rows**: structured and raw `WHERE` filters, keyset paging, exact counts on request. Values are read by storage class and the
  declared type: `DATE`/`DATETIME` text becomes a date or timestamp, `BOOLEAN` 0/1 a boolean, `DECIMAL` a number, `JSON` text JSON,
  `BLOB` bytes.
- **Edits**: by primary key, or by `rowid` for a table without one (browsed as the extra column `_rowid_`). A `WITHOUT ROWID` table
  always has a primary key, so it is edited like any keyed table. Views have no key and are read-only; so is every table of a
  database opened read-only. SQLite has no row versions: an update or delete compares the original values with `IS` (NULL-safe),
  and a change that matches no row rolls the save back with a `CONFLICT` (the row as it is now, or "no longer exists").
  Foreign keys are enforced (`PRAGMA foreign_keys = ON`).
- **Query tabs**: one connection per tab. Stop interrupts the running statement (`sqlite3_interrupt`), the statement timeout is a
  watchdog that interrupts it, read-only is `PRAGMA query_only`. Errors carry the extended result code and, for statements that do
  not compile, the position.
- **Explain**: `invoke("sqlite.explain", { sql })` returns `EXPLAIN QUERY PLAN` as `{ plan: [{ id, parent, detail }], sql }`; it never
  runs the statement, and the gateway treats it as a read.
- **Relations**: foreign keys in both directions and the ER diagram of a database.
- **Object actions**: design, copy name, explain a select, analyze, delete all rows (danger), reindex, drop trigger (danger).

## Not done

Extension loading is not compiled in. Encrypted databases (SQLCipher) are not supported. A shared in-memory database uses
SQLite's shared cache, so a long write in one tab can make another connection wait for the table lock.

## Running the contract tests

No container is needed; the suites write a file in the temp folder (`fosta-contract-sqlite.db`, or `FOSTA_CONTRACT_SQLITE_FILE`):

```sh
cd src-tauri && cargo test contract_sqlite -- --ignored --test-threads=1
```

`ci/engines/sqlite.json` reuses the PostgreSQL compose file only because `live.yml` starts a compose file for every entry.
