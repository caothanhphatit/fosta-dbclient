/**
 * A pretend SQL engine over a `MockCatalog`, enough for the query tab to feel real: SELECT from a table (columns, WHERE,
 * ORDER BY, LIMIT / TOP), SELECT of literals and a few functions, SHOW DATABASES / TABLES, USE, transactions, writes and
 * DDL (reported, not applied), `SELECT SLEEP(n)` / `WAITFOR DELAY` to try Stop, `SELECT * FROM seq_1_to_5000` /
 * `generate_series(1, 5000)` for the row limit, PRINT as a notice, and engine-style errors with the
 * statement and its position for anything else.
 */
import type { ColumnInfo, RunRequest, StreamEvent, Value } from "../../core/contract";
import { splitStatements, type SplitOptions } from "../../core/language";
import type { StatementCategory, StatementInfo } from "../../core/types";
import { column, compareValues, rawSqlFilter, v, type MockCatalog } from "./catalog";
import { positionAt } from "./errors";
import type { RunContext } from "./types";

export interface SqlErrors {
  syntax(near: string, line: number): Record<string, unknown> & { code: string; message: string };
  noTable(name: string): Record<string, unknown> & { code: string; message: string };
  noColumn(name: string, where: string): Record<string, unknown> & { code: string; message: string };
  cancelled(): Record<string, unknown> & { code: string; message: string };
}

export interface SqlDialectMock {
  split: SplitOptions;
  errors: SqlErrors;
  version: string;
  /** MariaDB/MySQL: DDL and BEGIN commit an open transaction first. */
  ddlCommits?: boolean;
  /** Rows a SELECT returns at most (the rest is `truncated`). */
  maxRows?: number;
}

const BATCH = 100;
const BATCH_DELAY_MS = 25;

function statement(category: StatementCategory, kind: string, tables: string[], excerpt: string, reason: string | null = null): StatementInfo {
  return { category, kind, tables, excerpt, reason };
}

/** Keyword classification, enough to show the gateway dialogs (the backend parses SQL properly). */
export function classifySql(text: string, split: SplitOptions = {}): StatementInfo[] {
  return splitStatements(text, split).map(({ from, to }) => {
    const sql = text.slice(from, to).trim();
    const first = sql.split(/\s+/)[0]?.toUpperCase() ?? "";
    const table = /(?:from|into|update|table)\s+[`"[]?([\w.]+)/i.exec(sql)?.[1] ?? "";
    const tables = table ? [table] : [];
    if (/^(begin|start|commit|rollback|savepoint)\b/i.test(sql)) return statement("transaction", first, [], sql);
    if (/^(grant|revoke|kill|create\s+user|set\s+global)\b/i.test(sql)) return statement("admin", first, tables, sql);
    if (/^(drop|truncate)\b/i.test(sql)) return statement("destructive", first, tables, sql, "Removes the object and its data");
    if (/^(delete|update)\b/i.test(sql) && !/\bwhere\b/i.test(sql))
      return statement("destructive", first, tables, sql, `${first} without WHERE affects every row`);
    if (/^(insert|update|delete|replace|merge)\b/i.test(sql)) return statement("write", first, tables, sql);
    if (/^(create|alter|rename)\b/i.test(sql)) return statement("ddl", first, tables, sql);
    return statement("read", first, tables, sql);
  });
}

/** Waits, failing with the engine's "cancelled" error when the run is stopped. */
export function pause(ms: number, context: RunContext, cancelled: () => unknown): Promise<void> {
  return new Promise((resolve, reject) => {
    if (context.signal.aborted) {
      reject(cancelled());
      return;
    }
    const timer = setTimeout(() => {
      context.signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(cancelled());
    };
    context.signal.addEventListener("abort", onAbort, { once: true });
  });
}

function literalColumn(expression: string, value: Value, name?: string): ColumnInfo {
  const kind = value.t === "int" ? "int" : value.t === "float" ? "float" : value.t === "decimal" ? "decimal" : value.t === "timestamp" ? "timestamp" : "text";
  return column(name ?? expression, kind === "int" ? "bigint" : kind === "timestamp" ? "datetime" : kind === "text" ? "varchar" : "double", kind, {
    nullable: true,
  });
}

/** Runs a script statement by statement, streaming events; resolves with the number of statements. */
export async function runSql(
  catalog: MockCatalog,
  dialect: SqlDialectMock,
  request: RunRequest,
  emit: (event: StreamEvent) => void,
  context: RunContext,
): Promise<number> {
  const { session } = context;
  const text = request.text;
  const ranges = splitStatements(text, dialect.split);
  const maxRows = dialect.maxRows ?? 1000;
  let database = request.database ?? session.database;
  for (const [index, range] of ranges.entries()) {
    const sql = text.slice(range.from, range.to);
    const started = performance.now();
    emit({ event: "statement", index, sql });
    await pause(60, context, dialect.errors.cancelled);
    const fail = (error: Record<string, unknown>, at = 0) => ({
      ...error,
      position: positionAt(text, range.from + at),
      statement: { index, text: sql.length > 200 ? `${sql.slice(0, 200)}…` : sql },
    });
    const done = (rows: number, affected: number | null, truncated = false) =>
      emit({ event: "done", index, affected, rows, truncated, elapsedMs: Math.max(1, Math.round(performance.now() - started)) });
    // Batches after the first arrive a little later, like a network stream, so the live row count can be seen.
    const result = async (columns: ColumnInfo[], rows: Value[][]) => {
      emit({ event: "columns", index, columns });
      const shown = rows.slice(0, maxRows);
      for (let start = 0; start < shown.length; start += BATCH) {
        if (start > 0) await pause(BATCH_DELAY_MS, context, () => fail(dialect.errors.cancelled()));
        emit({ event: "rows", index, rows: shown.slice(start, start + BATCH) });
      }
      done(shown.length, null, rows.length > maxRows);
    };

    let match: RegExpExecArray | null;
    if (dialect.ddlCommits && session.transaction.open && /^(create|alter|drop|truncate|rename|start\s+transaction|begin)\b/i.test(sql)) {
      // MariaDB/MySQL commit the open transaction before DDL (and a new BEGIN).
      session.transaction = { ...session.transaction, open: false };
      emit({ event: "notice", index, text: "The open transaction was committed implicitly before this statement" });
    }
    if (/^(begin|start\s+transaction|begin\s+tran(saction)?)\b/i.test(sql)) {
      session.transaction = { ...session.transaction, open: true };
      done(0, null);
    } else if (/^(commit|rollback)\b/i.test(sql)) {
      session.transaction = { ...session.transaction, open: false };
      done(0, null);
    } else if ((match = /^use\s+[`"[]?([\w-]+)[`"\]]?$/i.exec(sql))) {
      const name = match[1];
      if (!catalog.databases.some((item) => item.name.toLowerCase() === name.toLowerCase())) throw fail(dialect.errors.noTable(name), 4);
      database = name;
      session.database = name;
      emit({ event: "notice", index, text: `Database changed to ${name}` });
      done(0, 0);
    } else if ((match = /^(?:select\s+sleep\(\s*(\d+(?:\.\d+)?)\s*\)|waitfor\s+delay\s+'(?:00:)?(?:00:)?(\d+)')$/i.exec(sql))) {
      const seconds = Number(match[1] ?? match[2]);
      await pause(seconds * 1000, context, () => fail(dialect.errors.cancelled()));
      await result([column("SLEEP", "bigint", "int", { nullable: true })], [[v.int(0)]]);
    } else if ((match = /^select\s+\*\s+from\s+(?:seq_1_to_(\d+)|generate_series\(\s*1\s*,\s*(\d+)\s*\))$/i.exec(sql))) {
      // MariaDB's sequence tables and SQL Server's GENERATE_SERIES: many rows, to try the row limit and live counts.
      const count = Math.min(Number(match[1] ?? match[2]), 1_000_000);
      const name = match[1] ? "seq" : "value";
      await result(
        [column(name, "bigint", "int")],
        Array.from({ length: Math.min(count, maxRows + 1) }, (_, i) => [v.int(i + 1)]),
      );
    } else if ((match = /^print\s+N?'((?:[^']|'')*)'$/i.exec(sql))) {
      // SQL Server PRINT: a server message, no result.
      emit({ event: "notice", index, text: match[1].replace(/''/g, "'") });
      done(0, null);
    } else if (/^show\s+databases$/i.test(sql)) {
      await result(
        [column("Database", "varchar(64)", "text")],
        catalog.databases.map((item) => [v.text(item.name)]),
      );
    } else if (/^show\s+(full\s+)?tables$/i.test(sql)) {
      await result(
        [column(`Tables_in_${database ?? ""}`, "varchar(64)", "text")],
        catalog.tablesOf(database ?? "").map((table) => [v.text(catalog.style.label(table.spec))]),
      );
    } else if (
      (match =
        /^select\s+(?:top\s+(\d+)\s+)?([\s\S]+?)\s+from\s+((?:[`"[]?[\w$-]+[`"\]]?\.){0,2}[`"[]?[\w$-]+[`"\]]?)(?:\s+(?:as\s+)?\w+)?(?:\s+where\s+([\s\S]+?))?(?:\s+order\s+by\s+([`"[]?\w+[`"\]]?)(\s+desc|\s+asc)?)?(?:\s+limit\s+(\d+)(?:\s+offset\s+(\d+))?)?$/i.exec(
          sql,
        ))
    ) {
      const [, top, list, name, where, orderBy, direction, limit, offset] = match;
      const table = catalog.resolve(name, database);
      if (!table) throw fail(dialect.errors.noTable(name), sql.toLowerCase().indexOf(name.toLowerCase()));
      let indexes = table.spec.columns.map((_, i) => i);
      if (list.trim() !== "*") {
        indexes = list.split(",").map((part) => {
          const columnName = part.trim().replace(/^[`"[]|[`"\]]$/g, "");
          const found = table.columnIndex(columnName);
          if (found < 0) throw fail(dialect.errors.noColumn(columnName, "field list"), sql.indexOf(part.trim()));
          return found;
        });
      }
      let rows = table.rows;
      if (where) {
        try {
          const predicate = rawSqlFilter(table, where, catalog.style);
          rows = rows.filter(predicate);
        } catch (error) {
          throw fail(error as Record<string, unknown>, sql.indexOf(where));
        }
      }
      if (orderBy) {
        const sortIndex = table.columnIndex(orderBy.replace(/^[`"[]|[`"\]]$/g, ""));
        if (sortIndex < 0) throw fail(dialect.errors.noColumn(orderBy, "order clause"), sql.indexOf(orderBy));
        const sign = /desc/i.test(direction ?? "") ? -1 : 1;
        rows = [...rows].sort((a, b) => compareValues(a[sortIndex], b[sortIndex]) * sign);
      }
      const skip = Number(offset ?? 0);
      const take = top ? Number(top) : limit ? Number(limit) : rows.length;
      rows = rows.slice(skip, skip + take);
      await result(
        indexes.map((i) => table.spec.columns[i]),
        rows.map((row) => indexes.map((i) => row[i])),
      );
    } else if ((match = /^select\s+([\s\S]+)$/i.exec(sql)) && !/\bfrom\b/i.test(sql)) {
      const items = match[1].split(",").map((item) => item.trim());
      const values = items.map((item): [string, Value] => {
        const [expression, alias] = item.split(/\s+as\s+/i).map((part) => part.trim());
        const name = alias?.replace(/^[`"[]|[`"\]]$/g, "") ?? expression;
        if (/^[+-]?\d+$/.test(expression)) return [name, v.int(Number(expression))];
        if (/^[+-]?\d*\.\d+$/.test(expression)) return [name, v.dec(expression)];
        const quoted = /^N?'((?:[^']|'')*)'$/.exec(expression);
        if (quoted) return [name, v.text(quoted[1].replace(/''/g, "'"))];
        if (/^(now\(\)|current_timestamp(\(\))?|getdate\(\)|sysdatetime\(\))$/i.test(expression))
          return [name, v.ts(new Date().toISOString().slice(0, 19).replace("T", " "))];
        if (/^(version\(\)|@@version)$/i.test(expression)) return [name, v.text(dialect.version)];
        if (/^(database\(\)|db_name\(\)|schema\(\))$/i.test(expression)) return [name, database ? v.text(database) : v.null()];
        if (/^null$/i.test(expression)) return [name, v.null()];
        throw fail(dialect.errors.noColumn(expression, "field list"), sql.indexOf(expression));
      });
      await result(
        values.map(([name, value]) => literalColumn(name, value)),
        [values.map(([, value]) => value)],
      );
    } else if (/^(insert|update|delete|replace|merge)\b/i.test(sql)) {
      const affected = /^insert/i.test(sql) ? 1 : 3;
      if (!session.transaction.autoCommit) session.transaction = { ...session.transaction, open: true };
      emit({ event: "notice", index, text: `${affected} row(s) affected (the dev mock does not change its data from SQL)` });
      done(0, affected);
    } else if (/^(create|alter|drop|truncate|rename|grant|revoke|set|exec|execute|go)\b/i.test(sql)) {
      emit({ event: "notice", index, text: "Query OK (the dev mock reports DDL without running it)" });
      done(0, 0);
    } else if (/^(explain|describe|desc)\b/i.test(sql)) {
      await result(
        [
          column("id", "bigint", "int"),
          column("select_type", "varchar", "text"),
          column("table", "varchar", "text"),
          column("type", "varchar", "text"),
          column("rows", "bigint", "int"),
        ],
        [[v.int(1), v.text("SIMPLE"), v.text("products"), v.text("ALL"), v.int(1240)]],
      );
    } else {
      const near = sql.slice(0, 40);
      throw fail(dialect.errors.syntax(near, positionAt(text, range.from).line));
    }
  }
  return ranges.length;
}
