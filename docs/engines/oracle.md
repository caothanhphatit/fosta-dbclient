# Oracle

Code: `src-tauri/src/engines/oracle/` (backend), `src/engines/oracle/` (UI), `src/dev/mock/engines/oracle.ts` (dev mock).
The driver is the `oracle` crate on ODPI-C, which loads Oracle Instant Client at runtime. The crate is synchronous, so every
call runs on Tokio's blocking pool.

## Oracle Instant Client

Connecting needs Instant Client 19c or newer. Before the first connection the app loads one, in this order:

1. the profile's **Instant Client folder** (Advanced section of the connection dialog; `options.clientDir`);
2. a system install: `PATH` (Windows), `LD_LIBRARY_PATH` (Linux), `DYLD_LIBRARY_PATH` (macOS), `ORACLE_HOME`;
3. a client Fosta Studio downloaded into its data folder (`fosta-studio-oracle/` next to `fosta-studio.json`, or
   `<FOSTA_STUDIO_DATA>/oracle` in the web edition).

ODPI-C loads a client once per process, so the first one loaded stays in use until the app restarts. A folder that does not
hold the client library (`oci.dll`, `libclntsh.so`) is an error of its own, not silently skipped.

Without a client, connecting fails with the code `ORACLE_CLIENT`, `kind` (`notInstalled`, `load`, `network`, `cancelled`,
`checksumMismatch`, `diskFull`, …) and `download` (the pinned package: version, size, URL and license link). The UI turns that
into a prompt: **Download** (Instant Client Basic Light 23.26.4 from download.oracle.com, about 60 MB on Windows x64, with a
progress bar and Cancel; a cancelled download resumes), **Use folder** (saved as the profile's Instant Client folder), or
**Cancel**. After a download or a folder the connect is retried. The prompt says that the download comes from Oracle and is
covered by the Oracle Instant Client license, with a link to the terms, before it starts.

Pinned packages (version, size and SHA-256 in `instant_client.rs`): Windows x64, Linux x64 and Linux arm64. The web edition
(Docker, Linux) downloads the Linux package on the server; Instant Client on Linux also needs `libaio` (`libaio1`, or
`libaio1t64` plus a `libaio.so.1` link on newer Debian and Ubuntu). macOS has no pinned package yet (Oracle ships a disk image):
the prompt then lists the manual steps. The Windows client needs the Microsoft Visual C++ 2015-2022 Redistributable.

## What it does

- **Connect**: host, port and service name (EZConnect), or a connect descriptor / TNS alias in Advanced. `oracle://` URLs,
  `user/pass@//host:1521/SERVICE` and `user/pass@alias` paste into the form. TLS is `tcps`, verified by Instant Client against its
  wallet (it cannot skip verification). SSH tunnels come from the connection manager.
- **Tree**: schemas (users that are not Oracle-maintained and own objects; the login schema first, marked "login schema") with
  folders for tables, views, materialized views, sequences, synonyms, packages, procedures and functions. Empty folders are left
  out; each level is one catalog query. Oracle's schemas are the "databases" of the shared UI: the query tab's database picker,
  the ER diagram and completion work per schema, and every object carries its owner as `ObjectRef.database`.
- **Definition view**: packages (spec and body), procedures and functions open their PL/SQL source; sequences and synonyms
  their settings and DDL. It shows the status (VALID / INVALID), timestamps and compile errors from `ALL_ERRORS`. The source comes
  from `DBMS_METADATA` (other schemas need `SELECT_CATALOG_ROLE`) or `ALL_SOURCE`, with a `/` after each unit so it runs as is.
  Actions: Source, Copy name, Compile (`ALTER … COMPILE`).
- **Rows**: structured and raw `WHERE` filters, keyset paging, exact counts on request and optimizer estimates otherwise
  (`EXPLAIN PLAN` for a filter). Values are typed: `NUMBER(p,0)` with p ≤ 18 is an integer, other numbers decimals, `DATE` and
  `TIMESTAMP` timestamps, `TIMESTAMP WITH (LOCAL) TIME ZONE` timestamps with an offset, `BOOLEAN`, `JSON` (read through
  `JSON_SERIALIZE`), `RAW` / `BLOB` bytes, `CLOB` text, intervals.
- **Edits**: by primary key, or by ROWID for a heap table without one (browsed as the read-only extra column `_rowid_`). A ROWID
  is stable while the row stays where it is; moving the row (`ALTER TABLE … MOVE`, partition changes, shrink, export and import)
  changes it, and an edit with the old ROWID then matches no row and is reported as a conflict. Index-organized and external
  tables without a key, views and materialized views are read-only.
- **Optimistic concurrency**: an update or delete compares the original values the grid sends, NULL-safe
  (`(col = :a OR (col IS NULL AND :b IS NULL))`). LOB, `LONG`, `JSON`, `XMLTYPE`, `BFILE` and object columns cannot be compared
  with `=` and are left out of the check. `ORA_ROWSCN` is not used: unless the table was created with `ROWDEPENDENCIES` (off by
  default, and fixed at `CREATE TABLE`), it is tracked per block, so a change to another row in the same block would look like a
  conflict. All changes of a save run in one transaction; a change that matches no row rolls the save back with a `CONFLICT`
  carrying the row as it is now (or "no longer exists"). "Overwrite" checks the key only.
- **Query tabs**: one Oracle session per tab. Each statement commits unless a transaction is begun from the toolbar (Begin or
  auto-commit off); the tab then reports the transaction as open until Commit or Rollback, even before the first change. DDL
  commits implicitly, as always in Oracle. The tab's database is the session's current schema (`ALTER SESSION SET CURRENT_SCHEMA`);
  a name that is not a schema, such as the service name, is ignored. PL/SQL blocks end at a `/` line; result sets returned with
  `DBMS_SQL.RETURN_RESULT` are shown as extra results. Native `JSON` columns arrive as JSON (the query is described with
  `DBMS_SQL` and wrapped in `JSON_SERIALIZE`; only a result with two columns of the same name falls back to an error that says how
  to select the column as text).
- **Cancel and timeouts**: Stop calls `OCIBreak` on the tab's session from another thread (`ORA-01013` becomes `CANCELLED`); the
  statement timeout is a watchdog that breaks the call and reports `TIMEOUT`. OCI's own call timeout is not used, because it leaves
  the session unusable.
- **Errors**: `ORA-nnnnn` is the vendor code; parse errors carry the position (Oracle reports a byte offset, converted to line,
  column and offset in the whole script) and the failing statement. Logon errors (`ORA-01017`, `ORA-12541`, …) are connection
  errors.
- **Safety classification**: the shared SQL rules plus PL/SQL. Anonymous blocks (`BEGIN`, `DECLARE`) are writes unless their text
  shows more: `DROP`, `TRUNCATE` or `PURGE` anywhere in the block (dynamic SQL in strings included) makes them destructive, and
  users, roles, grants or `ALTER SYSTEM` / `ALTER DATABASE` administrative. `DROP` and `TRUNCATE` are destructive, `ALTER SYSTEM`
  administrative (both need the typed confirmation in a guarded workspace and are blocked in a read-only one); `ALTER SESSION` is a
  session setting.
- **Relations**: foreign keys in both directions (referencing tables in any schema the user can see) and the ER diagram of a
  schema.

## Not done

- `OCIBreak` interrupts SQL, but some PL/SQL waits (for example `DBMS_SESSION.SLEEP`) only end when they finish.
- Sessions have no read-only switch (`SET TRANSACTION READ ONLY` would end at the first commit); the workspace's safety rules block
  writes instead.
- No pinned Instant Client for macOS; the thin protocol (no Instant Client at all) is a later step.
- Object types, `XMLTYPE` and `SDO_GEOMETRY` columns are shown as text and are not compared in edits.

## Running the contract tests

Start the container from the main checkout (`docker compose -f dev/compose/oracle.yml up -d --wait`; ~2 GB RAM) and get a
client: on PATH, or downloaded once into the test binary's data folder:

```sh
cd src-tauri
cargo test live_oracle_instant_client_download -- --ignored --nocapture
cargo test contract_oracle -- --ignored --test-threads=1
```

`FOSTA_CONTRACT_ORACLE_CLIENT_DIR` passes a client folder as the profile's Instant Client folder;
`FOSTA_CONTRACT_ORACLE_{HOST,PORT,USER,PASSWORD,SERVICE}` override the server (defaults: `127.0.0.1:1521`, `shop` /
`studio-root`, `FREEPDB1`). The suite data is `dev/seed/oracle/contract.sql` (run before each suite); the core's own live tests
(`cargo test live_engine_oracle -- --ignored --test-threads=1`) use `dev/seed/oracle.sql`.
