// Statement splitting and "statement at cursor", the TypeScript twin of `src-tauri/src/lang/split.rs`.
// `;` ends a statement except inside strings, comments, quoted names and bodies with their own `;` (PostgreSQL
// BEGIN ATOMIC, SQLite triggers, MySQL routines and DELIMITER scripts, SQL Server BEGIN/END and module batches,
// Oracle PL/SQL up to a `/` line). Ranges exclude the terminating `;` (PL/SQL keeps its `END;`).
import type { LangDialect, TextRange } from "../types";
import { isMysql, isTrivia, tokenize, type Token } from "./lexer";

type Block = "plain" | "counted" | "plsql" | "batch";

const MSSQL_BEGIN_WITHOUT_END = ["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG", "CONVERSATION"];

const isWord = (t: Token | undefined, w: string) => !!t && t.kind === "word" && t.text.toUpperCase() === w;
const isSemi = (t: Token | undefined) => !!t && t.kind === "punct" && t.text === ";";

/** The ranges of the statements of `src`, in order. Empty statements are dropped. */
export function splitStatements(dialect: LangDialect, src: string): TextRange[] {
  const tokens = tokenize(dialect, src).filter((t) => !isTrivia(t));
  const out: TextRange[] = [];
  let stmtStart: number | null = null;
  let block: Block = "plain";
  let depth = 0;
  let delimiter: string | null = null;
  const finish = (from: number, end: number) => {
    const start = tokens[from].start;
    if (end > start) out.push({ from: start, to: end });
  };
  let i = 0;
  while (i < tokens.length) {
    const t = tokens[i];
    if (t.kind === "slashLine" || t.kind === "batchSeparator" || t.kind === "clientCommand") {
      if (stmtStart !== null) finish(stmtStart, tokens[i - 1].end);
      stmtStart = null;
      if (t.kind === "clientCommand") {
        const delim = t.text.slice(9).trim();
        delimiter = delim && delim !== ";" ? delim : null;
      }
      i++;
      continue;
    }
    if (delimiter !== null) {
      let pos = -1;
      if (t.kind === "word" || t.kind === "operator" || t.kind === "punct") pos = src.startsWith(delimiter, t.start) ? 0 : t.text.indexOf(delimiter);
      if (pos >= 0) {
        const cut = t.start + pos;
        if (stmtStart !== null) {
          if (pos > 0) finish(stmtStart, cut);
          else if (stmtStart < i) finish(stmtStart, tokens[i - 1].end);
        }
        stmtStart = null;
        const delimEnd = cut + delimiter.length;
        while (i < tokens.length && tokens[i].end <= delimEnd) i++;
        continue;
      }
      if (stmtStart === null) stmtStart = i;
      i++;
      continue;
    }
    if (stmtStart === null) {
      if (isSemi(t)) {
        i++;
        continue;
      }
      stmtStart = i;
      block = detectBlock(dialect, tokens.slice(i, i + 16));
      depth = 0;
    }
    const semi = isSemi(t);
    if (block === "counted") {
      if (t.kind === "word") depth += depthChange(dialect, tokens, i, depth);
      if (semi && depth <= 0) {
        finish(stmtStart, tokens[i - 1].end);
        stmtStart = null;
      }
    } else if (block === "plain" && semi) {
      if (stmtStart < i) finish(stmtStart, tokens[i - 1].end);
      stmtStart = null;
    }
    i++;
  }
  if (stmtStart !== null) {
    const last = tokens.length - 1;
    const end = isSemi(tokens[last]) && block !== "plsql" && block !== "batch" && last > stmtStart ? tokens[last - 1].end : tokens[last].end;
    finish(stmtStart, end);
  }
  return out;
}

function depthChange(d: LangDialect, tokens: Token[], i: number, depth: number): number {
  const next = tokens[i + 1];
  const word = tokens[i].text.toUpperCase();
  if (word === "BEGIN") {
    if (d === "postgres") return isWord(next, "ATOMIC") ? 1 : 0;
    if (d === "msSql") return next && ((next.kind === "word" && MSSQL_BEGIN_WITHOUT_END.includes(next.text.toUpperCase())) || isSemi(next)) ? 0 : 1;
    if (isMysql(d)) return isWord(next, "WORK") || isSemi(next) ? 0 : 1;
    return 1;
  }
  if (word === "CASE" && depth > 0) return isWord(tokens[i - 1], "END") ? 0 : 1;
  if (word === "END" && depth > 0) {
    if (isMysql(d) && ["IF", "LOOP", "WHILE", "REPEAT"].some((w) => isWord(next, w))) return 0;
    return -1;
  }
  return 0;
}

function detectBlock(d: LangDialect, toks: Token[]): Block {
  const words: string[] = [];
  for (const t of toks) {
    if (t.kind === "punct" && (t.text === "(" || t.text === ";")) break;
    if (t.kind === "word") words.push(t.text.toUpperCase());
  }
  const first = words[0];
  const creates = (kinds: string[], within: number) => first === "CREATE" && words.slice(1, 1 + within).some((w) => kinds.includes(w));
  switch (d) {
    case "oracle": {
      if (first === "DECLARE" || first === "BEGIN") return "plsql";
      if (first === "CREATE") {
        const rest = words.slice(1).filter((w) => !["OR", "REPLACE", "EDITIONABLE", "NONEDITIONABLE"].includes(w));
        if (["PROCEDURE", "FUNCTION", "PACKAGE", "TRIGGER", "TYPE", "LIBRARY"].includes(rest[0])) return "plsql";
      }
      return "plain";
    }
    case "sqlite":
      return creates(["TRIGGER"], 3) ? "counted" : "plain";
    case "postgres":
      return creates(["FUNCTION", "PROCEDURE"], 3) ? "counted" : "plain";
    case "mySql":
    case "mariaDb":
      return creates(["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"], 8) || (first === "BEGIN" && words[1] === "NOT" && words[2] === "ATOMIC")
        ? "counted"
        : "plain";
    case "msSql": {
      const module = ["PROC", "PROCEDURE", "FUNCTION", "TRIGGER", "VIEW"];
      let defines = false;
      if (first === "CREATE") defines = module.includes(words.slice(1).filter((w) => w !== "OR" && w !== "ALTER")[0]);
      else if (first === "ALTER") defines = module.includes(words[1]);
      return defines ? "batch" : "counted";
    }
    default:
      return "plain";
  }
}

/** End of a statement including a `;` right after it on the same line. */
function terminatorEnd(src: string, end: number): number {
  let j = end;
  while (j < src.length && (src[j] === " " || src[j] === "\t")) j++;
  return src[j] === ";" ? j + 1 : end;
}

const linesBetween = (src: string, a: number, b: number) => {
  let n = 0;
  for (let i = Math.min(a, b); i < Math.max(a, b); i++) if (src.charCodeAt(i) === 10) n++;
  return n;
};

/**
 * The statement under `offset`. Between statements it picks the one ending on the cursor's line, else the nearest by
 * lines (the previous one on a tie). Returns null when the text holds no statement.
 */
export function statementRangeAt(dialect: LangDialect, src: string, offset: number): TextRange | null {
  const ranges = splitStatements(dialect, src);
  const pos = Math.max(0, Math.min(offset, src.length));
  if (ranges.length === 0) return null;
  const inside = ranges.find((r) => r.from <= pos && pos <= terminatorEnd(src, r.to));
  if (inside) return inside;
  let prev: TextRange | null = null;
  let next: TextRange | null = null;
  for (const r of ranges) {
    if (r.to <= pos) prev = r;
    else if (r.from > pos && !next) next = r;
  }
  if (prev && next) {
    const dp = linesBetween(src, terminatorEnd(src, prev.to), pos);
    const dn = linesBetween(src, pos, next.from);
    return dp === 0 || dp <= dn ? prev : next;
  }
  return prev ?? next;
}

/** `LanguageSupport.statementAt`: the statement under the cursor, or an empty range at the cursor. */
export function statementAt(dialect: LangDialect, src: string, offset: number): TextRange {
  return statementRangeAt(dialect, src, offset) ?? { from: offset, to: offset };
}
