# Roadmap

Where Fosta Studio is heading. Plans change; open an issue to discuss an item or to offer help before starting on it.

## Available today

- MariaDB, MySQL, SQL Server and MongoDB, with connection strings and passwords kept in the OS credential store.
- Workspaces with their own connections, startup tasks (such as `kubectl port-forward`) and tabs.
- A fast data grid: inline editing applied in one transaction, column filters, show/hide columns, a cell editor for long values and
  JSON, a record panel with foreign-key navigation.
- Query tabs with history, an interactive ER diagram, import (CSV, JSON) and export (CSV, Excel CSV, JSON, SQL, Markdown).
- An AI assistant with the model of your choice that writes queries and opens views but never runs anything.
- MariaDB vector search tools.

## Next

- **Safety for every database**: rules per workspace (open, guarded, read-only), confirmations for risky statements, read-only
  Production workspaces, transactions with Begin / Commit / Rollback in query tabs, statement timeouts.
- **Docker image**: run Fosta Studio in a browser with one `docker run`.
- **More databases**: PostgreSQL, SQLite, ClickHouse, Oracle, Redis, and Elasticsearch / OpenSearch with search, ES|QL, mappings,
  aggregations and cluster insight.
- **Faster big results**: streaming results that show the first rows at once and stop when you press Stop.

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
- **macOS and Linux** builds, signed installers and automatic updates.
- **Plugins** for community-built database engines.
