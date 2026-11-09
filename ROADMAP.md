# Roadmap

Where Fosta Studio is heading. Plans change; open an issue to discuss an item or to offer help before starting on it.

## Available today

- Eleven engines: MariaDB, MySQL, SQL Server, MongoDB, PostgreSQL, SQLite, Redis (and Valkey), Elasticsearch, OpenSearch, ClickHouse and
  Oracle, with connection strings, an SSH tunnel and TLS, and passwords kept in the OS credential store.
- Workspaces with their own connections, commands (such as `kubectl port-forward`) and tabs.
- A fast data grid: inline editing checked against the values you loaded (a conflict dialog instead of a silent overwrite), column
  filters, show/hide columns, a cell editor for long values and JSON, a record panel with foreign-key navigation.
- Query tabs with column-aware completion, a formatter, streaming results with a real Stop, history, an interactive ER diagram, import
  (CSV, JSON) and export (CSV, Excel CSV, JSON, SQL, Markdown).
- Safety rules on every engine: rules per workspace (open, guarded, read-only), confirmations for risky statements, read-only
  Production workspaces, transactions with Begin / Commit / Rollback, statement timeouts and a local audit log.
- Structured errors with an error panel, an application log and copyable diagnostics; compact density and a richer status bar.
- A built-in AI agent with the model of your choice: it explores your schema, writes and explains queries, runs code blocks under the
  safety rules, optionally runs read-only queries itself, and opens views for you.
- Vector search tools for MariaDB and PostgreSQL (pgvector).
- A web edition: one Docker image to use Fosta Studio in a browser.

## Next

- **Oracle thin protocol**: connect without Oracle Instant Client.
- **Redis Cluster and Sentinel**: today Redis connects to a single node.
- **Code signing** for the Windows executable, and packages for **winget**, **Scoop** and **npm**.
- **MySQL in CI**: a MySQL container next to the MariaDB one in the live test suite.

## Later

- **MongoDB beyond Compass**: a query bar with autocomplete and shell syntax, document views (list, JSON, table), an aggregation
  builder with a preview per stage, schema analysis, visual explain plans.
- **Schema compare and sync** between environments, with migration scripts.
- **Visual execution plans** for every engine, with index suggestions.
- **Activity monitor**: running queries, locks and blocking sessions.
- **Table designer**, data transfer between engines, data profiling and test-data generation, backup and restore.
- **Notebooks** mixing SQL, notes and charts; quick charts from any result.
- **Local database servers**: create, start and stop development databases from the app, with or without Docker.
- **Local analytics** over results and files (CSV, Parquet, JSON).
- **Accounts and sync** (optional): encrypted sync of workspaces between machines, team sharing.
- **macOS and Linux** builds, installers and automatic updates.
- **Plugins** for community-built database engines.
