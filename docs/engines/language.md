# Language support

The query editor's language library: SQL formatting, statement at cursor, schema-driven completion for every SQL dialect, and completion for the MongoDB and Redis consoles.

- Rust: `src-tauri/src/lang/` (formatter, splitter, completion schema types). It depends on nothing else in the crate.
- TypeScript: `src/lang/` (CodeMirror 6 completion sources, statement at cursor, `LanguageSupport` factories). Import from `src/lang/index.ts`.

## Dialects

Both sides use the engine contract's `SqlDialectId` names:

| Id           | Engines              | Lexical rules worth knowing                                                                                 |
| ------------ | -------------------- | ----------------------------------------------------------------------------------------------------------- |
| `mySql`      | MySQL                | backticks, `"..."` is a string, backslash escapes, `#` comments, `--` needs a space after it, `DELIMITER x` |
| `mariaDb`    | MariaDB              | as MySQL, plus `BEGIN NOT ATOMIC` blocks                                                                    |
| `msSql`      | SQL Server           | `[names]`, `N'...'`, `@var`, `#temp`, nested block comments, `GO [n]` batch lines                           |
| `postgres`   | PostgreSQL           | dollar quotes, `E'...'`, `U&'...'`, `$1`, `::` casts, nested block comments, `BEGIN ATOMIC` bodies          |
| `sqlite`     | SQLite               | `"..."`, backticks and `[...]` names, `?1` `:name` `@name` `$name` parameters, trigger bodies               |
| `clickHouse` | ClickHouse           | backticks, backslash escapes, `# ` comments, `{name:Type}` parameters, heredocs, case-sensitive functions   |
| `oracle`     | Oracle               | `q'[...]'` strings, `:bind`, PL/SQL blocks that run to a `/` line                                           |
| `generic`    | anything else (ANSI) | `"names"`, `'strings'`, `--` and `/* */` comments                                                           |

Rust accepts the usual aliases too (`mysql`, `mssql`, `tsql`, `pg`, `postgresql`, `clickhouse`). In TypeScript, `dialectFor(engineId)` maps an engine or driver id to a dialect.

## Backend commands

The commands are declared in `commands/lang.rs` (owned by the core). Their bodies call this library.

### `format_sql`

Args (camelCase JSON): `{ dialect, text, options? }`. Returns the formatted text.

```rust
use crate::lang::{format_sql, Dialect, FormatOptions};

let out = format_sql(dialect, &text, &options.unwrap_or_default());
```

`FormatOptions` (all optional in JSON):

| Field         | Default   | Meaning                                                    |
| ------------- | --------- | ---------------------------------------------------------- |
| `indent`      | `2`       | spaces per level                                           |
| `useTabs`     | `false`   | indent with tabs                                           |
| `keywordCase` | `"upper"` | `"upper"`, `"lower"` or `"preserve"`                       |
| `maxLine`     | `100`     | break clauses and groups longer than this (0: never break) |

The formatter never changes what a statement means:

- It works on a lossless token stream. Strings, quoted names, comments and script lines (`GO`, `/`, `DELIMITER`) come out byte for byte, in their original order.
- Only whitespace changes, plus the case of keywords. Function names, type names and identifiers keep the case they were typed in. A word counts as a keyword only where it cannot be an identifier (see `lang/keywords.rs`).
- Every result is verified before it is returned: re-lexing it must give the same tokens as the input, up to whitespace and keyword case. `format_sql` returns the input unchanged if the check fails, and `try_format` returns `FormatError::Changed`. A fuzz test formats thousands of random token mixes in every dialect without one fallback.

Layout:

- Every clause starts a line: `SELECT`, `FROM`, joins, `WHERE`, `GROUP BY`, `ORDER BY`, `LIMIT`, set operators, `INSERT INTO`, `VALUES`, `SET`, `RETURNING`, and so on.
- A clause that does not fit on one line breaks after each top-level comma (`SELECT`, `FROM`, `ORDER BY`...) or before each top-level `AND`/`OR`/`ON` (`WHERE`, `HAVING`, joins).
- Parentheses stay on one line when they fit. Otherwise a subquery is laid out one level deeper, and lists put one item per line (lists of single values are packed instead).
- `CASE` breaks before each `WHEN`/`ELSE` when it does not fit.
- Procedural blocks indent their bodies: `BEGIN ... END`, `BEGIN TRY/CATCH`, PL/SQL `DECLARE`/`IS`, `IF ... THEN ... ELSE ... END IF`, loops, `EXCEPTION` handlers. PostgreSQL function bodies inside dollar quotes are strings and are left alone.
- Comments stay where they were relative to the code, and at most one blank line is kept between statements.

Why not a crate? `sqlformat` 0.5 (the maintained Rust port of sql-formatter) knows three dialects. It reads `#` as a comment everywhere, which breaks PostgreSQL's `#>` and `#` operators. It also has no Oracle `q'...'` strings or `/` lines, no ClickHouse escapes, and no `GO` handling. Pretty-printing through `sqlparser` drops comments and rewrites the SQL. Neither can give the "never change semantics" guarantee.

### `completion_schema`

Args: `{ connectionId, database }`. Returns a `CompletionSchema`:

```json
{
  "databases": ["shop"],
  "schemas": ["public", "sales"],
  "defaultSchema": "public",
  "tables": [
    {
      "name": "orders",
      "schema": "public",
      "kind": "table",
      "columns": [
        { "name": "id", "type": "int", "pk": true },
        { "name": "customer_id", "type": "int", "pk": false, "fk": { "table": "customers", "column": "id" } }
      ]
    }
  ],
  "truncated": false
}
```

The command body builds it from `tree_children` and `describe_object`:

```rust
use crate::lang::{schema::ForeignKeyPart, CompletionSchema, CompletionTable};

let mut schema = CompletionSchema { databases, default_schema, ..Default::default() };
schema.push_table(CompletionTable::from_parts(
    description.object.schema.as_deref(),
    &description.object.name,
    "table",
    description.columns.iter().map(|c| (c.name.as_str(), c.type_name.as_str())),
    &primary_key_columns,
    &foreign_keys, // ForeignKeyPart { columns, ref_schema, ref_table, ref_columns }
));
```

`push_table` stops at `schema::MAX_TABLES` (5,000) and sets `truncated`. For MongoDB, tables are collections and columns are sampled field paths. Redis does not use it.

### Statements

`lang::split(dialect, text) -> Vec<Statement>` and `lang::statement_at(dialect, text, byte_offset) -> Option<Statement>` give `{ text, start, end }` byte ranges without the terminating `;`. PL/SQL keeps its `END;`. Bodies that contain `;` stay whole:

- PostgreSQL `BEGIN ATOMIC`;
- SQLite triggers;
- MySQL routines, `BEGIN NOT ATOMIC` and `DELIMITER` scripts;
- SQL Server `BEGIN ... END` (not `BEGIN TRAN`) and `CREATE/ALTER PROCEDURE|FUNCTION|TRIGGER|VIEW`, which run to `GO`;
- Oracle PL/SQL, which runs to a `/` line.

Between statements, the cursor picks the statement that ends on its line, else the nearest one by lines (the previous one on a tie).

## TypeScript API

### `LanguageSupport`

```ts
interface LanguageSupport {
  id: string; // "sql" | "mongo-command" | "redis"
  editorExtensions(ctx?: { connectionId?: string; database?: string | null }): Promise<Extension[]>;
  statementAt(text: string, offset: number): { from: number; to: number };
  format?(text: string): Promise<string>;
}
```

This is the hook the query view uses. "Run current statement" runs `statementAt`, or the selection when there is one (see `currentStatement`). "Format" calls `format`.

```ts
import { createSqlLanguage, createMongoLanguage, createRedisLanguage, dialectFor, currentStatement, formatInEditor } from "../lang";

const sql = createSqlLanguage({ dialect: dialectFor(engineId) });
const extensions = await sql.editorExtensions({ connectionId, database }); // also starts loading completion_schema
// Run current statement:
const stmt = currentStatement(view.state, sql.statementAt); // { from, to, text } | null
// Format button / shortcut: formats the selection or the whole document in one undoable change.
await formatInEditor(view, sql);
// After DDL or a database switch:
sql.schema.set(await fetchCompletionSchema(connectionId, database));
```

- `createSqlLanguage({ dialect, schema?, format?, formatOptions? })`: highlighting for the dialect (lang-sql's dialects, plus a ClickHouse one), the completion source, `statementAt`, and `format` through `format_sql`. Pass a `SchemaCache` to share or control the schema. Without one, the language creates its own and loads it from `completion_schema` when `editorExtensions` gets a `connectionId`.
- `createMongoLanguage({ schema? })`: completion and statements for the Mongo console.
- `createRedisLanguage({ keys? })`: command completion, with `keys()` offered at key positions. The current line is the statement.

The pieces are exported separately too: `sqlCompletionSource`, `sqlCompletions` (pure, for tests or other editors), `analyzeSql`, `splitStatements`, `statementRangeAt`, `quoteIfNeeded`, `mongoCompletionSource`, `redisCompletionSource`, `arityError`, `REDIS_COMMANDS`.

### SQL completion

`analyzeSql(dialect, schema, doc, pos)` finds the statement under the cursor and the word being typed (an opening quote counts). It classifies the position as one of these kinds:

| Kind             | Where                                                | Offers                                                                                 |
| ---------------- | ---------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `statementStart` | nothing typed yet                                    | statement keywords first                                                               |
| `table`          | after `FROM`, `INTO`, `UPDATE`, `TABLE`, `,` in FROM | tables (schema-qualified outside the default schema), CTEs, schemas                    |
| `join`           | after `JOIN`                                         | FK joins first (`customers c ON c.id = o.customer_id`), then tables                    |
| `joinCondition`  | after `ON`, or `AND`/`OR` inside it                  | FK conditions for the table just joined, then columns                                  |
| `member`         | after `x.`                                           | columns of alias/table/CTE/subquery `x`, tables of schema `x`, schemas of database `x` |
| `column`         | expressions                                          | columns in scope, aliases, functions, keywords                                         |
| `afterTable`     | after a table name or alias                          | `AS`, `WHERE`, `JOIN`, `ON`...                                                         |
| `afterValue`     | after a value                                        | `FROM`, `AS`, `AND`, `IS NULL`...                                                      |
| `none`           | inside strings and comments                          | nothing                                                                                |

The scope comes from every query level of the statement. It holds FROM/JOIN/UPDATE/INTO refs with `AS` or implicit aliases, `schema.table` and quoted names. Subqueries in FROM carry their output columns (aliases, plain columns, `*` and `t.*` expanded). CTEs carry explicit column lists or derived columns, and a CTE may select from an earlier one. A correlated subquery also sees the outer refs, ranked below its own. A column name found in several tables is offered qualified (`o.id`, `c.id`).

Identifiers are quoted only when needed, the way the dialect quotes them: backticks for MySQL, MariaDB and ClickHouse, `[...]` for SQL Server, `"..."` elsewhere. A name needs quotes when:

- it is not a plain identifier;
- it is a reserved word;
- it has upper case in PostgreSQL;
- it has lower case in Oracle.

If the user already typed the opening quote, completion only closes it. Keywords follow the case typed (`sel` gives `select`). ClickHouse function names keep their camelCase.

Typing only opens the list once a letter is typed, or right after a `.`. Ctrl-Space opens it anywhere.

### Mongo completion

The completion works with both syntaxes the console takes: shell syntax (`db.orders.find({...})`, `db.getCollection("x")`) and JSON database commands (`{ "find": "orders", "filter": {...} }`). It tracks objects, arrays and calls up to the cursor and works out what goes there:

| Where                                                                     | Offers                                                                      |
| ------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `db.`                                                                     | collections (as `getCollection("...")` when the name needs it), db methods  |
| `db.coll.` and `getCollection(...).`                                      | collection methods                                                          |
| after `find(...)` or `aggregate(...)`                                     | cursor methods                                                              |
| inside `getCollection("`, a command value, `$lookup.from`                 | collection names                                                            |
| filter of `find`/`update*`/`delete*`/`count`, `$match`, `"filter"`, `"q"` | fields and `$and` `$or` `$nor` `$expr`...                                   |
| `{ field: { ` in a filter                                                 | `$eq` `$gt` `$gte` `$in` `$exists` `$regex` `$elemMatch`...                 |
| second argument of `update*`, `"u"`                                       | `$set` `$inc` `$push` `$unset`..., then fields                              |
| `aggregate([ {`, `"pipeline"`, `$facet`, `$lookup.pipeline`               | stages: `$match` `$group` `$lookup` `$unwind` `$project`...                 |
| `$group`                                                                  | `_id`, then accumulators (`$sum` `$avg` `$push`...) under each output field |
| `$project`/`$addFields`/`$set`, expressions                               | expression operators and fields                                             |
| `$lookup`, `$unwind`, `$merge`...                                         | the stage's option keys                                                     |
| a string starting with `$` in a value                                     | field paths                                                                 |
| first key of a JSON command                                               | command names, then that command's options                                  |

Keys are inserted bare in shell syntax when they are JavaScript identifiers (`$match`, `status`). Otherwise, and always in JSON commands, they are inserted quoted. Fields come from the collection's columns in the completion schema. `mongoStatements` splits on `;` and on top-level line breaks, unless the next line continues the expression (`.sort(...)`).

### Redis completion

`REDIS_COMMANDS` covers about 240 commands and subcommands. Each one has its `COMMAND INFO` arity, group, redis.io argument notation and summary, and container commands (`CONFIG`, `CLIENT`, `XGROUP`, `OBJECT`, `MEMORY`, `SLOWLOG`, `SCRIPT`, `FUNCTION`, `ACL`...) list their subcommands. Completion works through the line:

1. The first word completes to a command name; the signature and summary appear as info.
2. For a container command, the second word completes to its subcommands.
3. Further words complete to the command's literal option tokens not used yet (`EX`, `NX`, `WITHSCORES`, `BYSCORE`...). At key positions, the known keys come first, quoted when they hold spaces or quotes.

`arityError(words)` checks a line's argument count (arity counts the subcommand word).

## Tests

- Rust: `cargo test lang::`. This covers lexer and splitter cases per dialect, the keyword rules and a formatter corpus per dialect. The corpus checks round trips, kept comments, idempotence and every option, plus a fuzz of random token mixes in all dialects and the expected layouts.
- TypeScript: `src/lang/**/*.test.ts`. This covers the lexer and splitter (twins of the Rust ones), scope and alias resolution, SQL suggestions (FK joins, quoting, ranking), the CodeMirror source itself, and Mongo and Redis completion.
