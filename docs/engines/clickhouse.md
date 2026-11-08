# ClickHouse

Fosta Studio talks to ClickHouse 25.8 (the version the suites run against; see "Versions") over the HTTP interface, with the
app's own `reqwest` and no extra client crate. The engine id is `clickhouse`; the code is in `src-tauri/src/engines/clickhouse/`
and the UI in `src/engines/clickhouse/`.

## Why HTTP

The HTTP interface (port 8123, or 8443 with TLS) works through proxies and on ClickHouse Cloud. Results are asked for as
`JSONCompactEachRowWithNamesAndTypes`: one JSON array per line after a line of names and one of types, so each row can be
decoded as soon as its line arrives, the types come with the data, and 64-bit integers, decimals and non-finite floats are quoted
so nothing loses precision. Every request carries a `query_id`, which is how Stop works (below).

## Connecting

| Field | Meaning |
|---|---|
| Host, port | The server (default 8123). Through an SSH tunnel the client keeps the host name in the URL and connects to the tunnel's local end, so certificate names still match. |
| User, password | Default user `default`. The password is kept in the secret store. |
| Database | The default database of the connection (`default` when empty). The tree lists every database. |
| Connect timeout | Seconds, 10 by default. |

URLs: `clickhouse://user:pass@host:8123/db?secure=true`, `ch://…`, or the HTTP interface's own `http(s)://host:port/?user=…`.

TLS means HTTPS. `require` encrypts without checking the certificate, `verifyFull` checks the chain and the name; a CA file, a pinned
SHA-256 fingerprint and "accept invalid certificates" work as for the other engines. `prefer` is treated as plain HTTP.

## The tree

Databases (the server's own last), then the folders **Tables**, **Views**, **Materialized views** and **Dictionaries** (empty
ones are left out), loaded one level at a time. A table shows its engine, its row count and its size on disk, summed over the
**active parts** in `system.parts`: `MergeTree · 1.2M rows · 50 MiB`. Tables whose engine cannot be edited carry a
**read-only** badge. The table menu has **Design** (columns, keys including the sorting key, skipping indexes, the
`CREATE` statement), **Parts and mutations** and **Truncate…**.

## Browsing

Rows are typed (`UInt64`, `Decimal`, `DateTime64`, `Array`, `Map`, `Enum`, `LowCardinality`, `Nullable`, `IPv4`…); 128- and
256-bit integers and big decimals come back as exact decimals. Column filters are structured and every value is bound as a
`{pN:Type}` query parameter, never spliced into the SQL; the filter box takes a raw `WHERE` condition. Paging is by keyset over
the table's sorting key (`PRIMARY KEY`, which is **not unique** in ClickHouse), with offset paging as the fallback. The total is
counted exactly when the table is small (or you press Count) and otherwise comes from the statistics, or from `EXPLAIN ESTIMATE`
under a filter.

## Editing rows: asynchronous mutations

ClickHouse has no transactional row updates, so editing works differently from the other SQL engines, and the grid says so in a
notice above the table:

- An update is `ALTER TABLE … UPDATE …` and a delete is a lightweight `DELETE FROM … WHERE …` (`ALTER TABLE … DELETE` on servers
  before 23.3). Both are **mutations**: ClickHouse rewrites the data parts in the background. Fosta Studio waits for each one
  (`mutations_sync = 2`), but they are not transactions and cannot be rolled back.
- Only **MergeTree-family** tables (`MergeTree`, `ReplacingMergeTree`, `Replicated…`, `Shared…`, …) have mutations. Every other
  table is **read-only**: views, materialized views, dictionaries, `Log`, `Memory`, `Distributed`, `Kafka`… The grid has no Add,
  Delete or Import for them and the backend refuses edits with a message that names the engine. Add rows to such tables with an
  `INSERT` statement in a query tab.
- There is no row version, so a change is guarded by the **values you loaded**: before anything is written, every update and delete is
  checked with `SELECT count() … WHERE <key> AND <original values>`. A row that is gone or changed is a **conflict** (shown with
  Reload and Overwrite, like in the other engines), and then nothing of the set is applied, inserts included. The mutation itself
  repeats the same `WHERE`, so a row that changes between the check and the mutation is skipped instead of overwritten.
- A key that matches **more than one row** is refused ("matched 2 rows"): the primary key is not unique. Change those rows with a
  statement.
- A set of changes is applied **one after another**. If the server fails halfway, the error says how many changes had already been
  applied. This is the one place where a conflict-free set is not all-or-nothing.
- ClickHouse rejects updates of columns in the sorting, partition or primary key; the error from the server is shown as it is.

## Query tabs

Scripts run **one statement per request** (the HTTP interface takes a single statement), in a tab-wide `session_id`, so `SET` and
temporary tables carry over between runs. Results stream line by line in batches; the row limit of the rules stops reading and
the server query is killed. **Stop** sends `KILL QUERY WHERE query_id = …` on a separate request. The rules' statement timeout is
the server's `max_execution_time` (`timeout_overflow_mode = throw`), and the read-only rule is `readonly = 2` (no writes, settings
still changeable). ClickHouse has no transactions: the toolbar shows none. A `SELECT … FORMAT CSV` (a statement with its own `FORMAT`) comes
back as one text cell. The query tab's database, or the one the run names, is the default database of the statement.

## Errors

The server's `Code: N. DB::Exception: …` text becomes a structured error: `vendorCode` is N, the message is the server's sentence,
and for syntax errors the **position** (1-based `failed at position` turned into a 0-based offset in the editor text), the
failing statement and the long "Expected one of…" list (kept as the error's detail). Code 394 is a cancelled statement, 159 a
timeout. A failure after the first rows were sent is read from the end of the stream (`http_write_exception_in_output_format = 0`
since 23.3), so it is an error and not a row.

## Statement classification

| Class | Statements |
|---|---|
| Read | `SELECT`, `WITH`, `SHOW`, `DESCRIBE`/`DESC`, `EXPLAIN`, `EXISTS` |
| Write | `INSERT`, `ALTER … UPDATE`, `ALTER … DELETE`, `DELETE FROM … WHERE` |
| DDL | `CREATE`, `RENAME`, `ALTER` that adds or modifies |
| Destructive | `DROP`, `TRUNCATE`, `ALTER … DROP/DETACH …` (columns, partitions), `DELETE FROM` without `WHERE` |
| Administrative | `SYSTEM …`, `KILL`, `OPTIMIZE`, `GRANT`, `CREATE USER`, `ATTACH`/`DETACH` |

The default "guarded" rules ask for a typed confirmation for administrative and destructive statements alike. The engine
operations behind the views (`clickhouse.parts`, `.mutations`, `.processes`, `.queryLog`) are reads; `clickhouse.kill` is administrative.

## Views

- **Table grid**: the shared grid with the mutation notice, or read-only for tables without mutations.
- **Parts and mutations** (table menu): the data parts with rows and bytes (inactive parts dimmed), and the mutations with their
  command, state and last failure, refreshed while one still runs, with **Kill** (`KILL MUTATION`, after the rules' confirmation).
- **Server** (connection menu): the **running queries** (`system.processes`, refreshed every 3 s) with **Stop**, and the **query log**
  (`system.query_log`: recent finished queries with duration, memory, rows read and written, user and the statement; failed
  only; 50, 100 or 500 rows). The log is written a few seconds after a query ends. When the server has the log switched off, the
  view says so instead of failing.

## Not supported yet

Cluster-aware features (`ON CLUSTER`, `clusterAllReplicas` browsing), the native TCP protocol, `RowBinary`, projections and
dictionary reload buttons in the UI (run `SYSTEM RELOAD DICTIONARY` in a query tab), per-table `TTL` editing, and a visual EXPLAIN. The ER
diagram is not offered: ClickHouse has no foreign keys.

## Versions

The contract suites (`cargo test contract_clickhouse -- --ignored --test-threads=1`) run against `clickhouse/clickhouse-server:25.8`
from `dev/compose/clickhouse.yml` (login `studio` / `studio-root`; `CLICKHOUSE_TAG` picks another image). Older servers fall back
to `ALTER TABLE … DELETE` below 23.3 and to the server's default error format; those paths are written but have not been run.
