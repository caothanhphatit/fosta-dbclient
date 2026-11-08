// A lossless SQL lexer, the TypeScript twin of `src-tauri/src/lang/lexer.rs` (same rules, UTF-16 offsets): every
// character belongs to one token, whitespace and comments included. The splitter and the completion analysis use it.
import type { LangDialect } from "../types";

export type TokenKind =
  | "whitespace"
  | "lineComment"
  | "blockComment"
  | "word"
  | "quotedIdent"
  | "string"
  | "number"
  | "operator"
  | "punct"
  | "param"
  | "variable"
  | "slashLine"
  | "batchSeparator"
  | "clientCommand";

export interface Token {
  kind: TokenKind;
  start: number;
  end: number;
  text: string;
}

export function isTrivia(t: Token): boolean {
  return t.kind === "whitespace" || t.kind === "lineComment" || t.kind === "blockComment";
}

export function isMysql(d: LangDialect): boolean {
  return d === "mySql" || d === "mariaDb";
}

const OPERATOR_CHARS = "+-*/<>=~!@#%^&|?:$\\`";
const KEEPS_TRAILING_SIGN = "~!@#%^&|`?";
const PUNCT = "()[]{},;.";

const isDigit = (c: string) => c >= "0" && c <= "9";
const isAlpha = (c: string) => (c >= "a" && c <= "z") || (c >= "A" && c <= "Z");
const isAlnum = (c: string) => isAlpha(c) || isDigit(c);
const isSpace = (c: string) => c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\f" || c === "\v";
const isHigh = (c: string) => c !== "" && c.charCodeAt(0) >= 0x80;

/** Tokenizes `src` without losing a character. Unterminated strings and comments run to the end. */
export function tokenize(dialect: LangDialect, src: string): Token[] {
  const out: Token[] = [];
  const n = src.length;
  let i = 0;
  while (i < n) {
    const [kind, rawEnd] = nextToken(dialect, src, i);
    const end = Math.min(Math.max(rawEnd, i + 1), n);
    out.push({ kind, start: i, end, text: src.slice(i, end) });
    i = end;
  }
  return out;
}

/** The tokens that are neither whitespace nor comments. */
export function significant(dialect: LangDialect, src: string): Token[] {
  return tokenize(dialect, src).filter((t) => !isTrivia(t));
}

const at = (s: string, i: number) => (i >= 0 && i < s.length ? s[i] : "");

function nextToken(d: LangDialect, s: string, i: number): [TokenKind, number] {
  const c = s[i];
  if (isSpace(c)) {
    let j = i;
    while (j < s.length && isSpace(s[j])) j++;
    return ["whitespace", j];
  }
  const lc = lineComment(d, s, i);
  if (lc !== null) return ["lineComment", lc];
  if (c === "/" && at(s, i + 1) === "*") return ["blockComment", blockComment(d, s, i)];
  const script = scriptLine(d, s, i);
  if (script) return script;
  const pre = prefixedString(d, s, i);
  if (pre !== null) return ["string", pre];
  if (c === "'") return ["string", skipQuoted(s, i + 1, "'", isMysql(d) || d === "clickHouse")];
  if (c === '"') {
    if (isMysql(d)) return ["string", skipQuoted(s, i + 1, '"', true)];
    return ["quotedIdent", skipQuoted(s, i + 1, '"', d === "clickHouse")];
  }
  if (c === "`" && (isMysql(d) || d === "sqlite" || d === "clickHouse")) return ["quotedIdent", skipQuoted(s, i + 1, "`", d === "clickHouse")];
  if (c === "[" && (d === "msSql" || d === "sqlite")) return ["quotedIdent", skipQuoted(s, i + 1, "]", false)];
  if (c === "$" && (d === "postgres" || d === "clickHouse") && !prevIsIdent(s, i)) {
    const end = dollarQuote(s, i);
    if (end !== null) return ["string", end];
  }
  const pv = paramOrVariable(d, s, i);
  if (pv) return pv;
  if (isDigit(c) || (c === "." && isDigit(at(s, i + 1)) && !prevIsIdent(s, i) && !")]\"`'".includes(at(s, i - 1) || "x"))) {
    return ["number", numberEnd(s, i)];
  }
  if (isWordStart(d, c)) return ["word", wordEnd(d, s, i)];
  if (PUNCT.includes(c)) return ["punct", i + 1];
  if (OPERATOR_CHARS.includes(c)) return ["operator", operatorEnd(d, s, i)];
  return ["operator", i + 1];
}

function lineComment(d: LangDialect, s: string, i: number): number | null {
  const c = s[i];
  let comment = false;
  if (c === "-" && at(s, i + 1) === "-") {
    const after = at(s, i + 2);
    comment = !isMysql(d) || after === "" || isSpace(after) || after.charCodeAt(0) < 32;
  } else if (c === "#" && isMysql(d)) comment = true;
  else if (c === "#" && d === "clickHouse") comment = at(s, i + 1) === " " || at(s, i + 1) === "!";
  return comment ? lineEnd(s, i) : null;
}

function lineEnd(s: string, i: number): number {
  let j = i;
  while (j < s.length && s[j] !== "\n") j++;
  if (j > i && s[j - 1] === "\r") j--;
  return Math.max(j, i + 1);
}

function blockComment(d: LangDialect, s: string, i: number): number {
  const nested = d === "postgres" || d === "msSql";
  let depth = 1;
  let j = i + 2;
  while (j < s.length && depth > 0) {
    if (s[j] === "*" && at(s, j + 1) === "/") {
      depth--;
      j += 2;
    } else if (nested && s[j] === "/" && at(s, j + 1) === "*") {
      depth++;
      j += 2;
    } else j++;
  }
  return Math.min(j, s.length);
}

function lineStartBefore(s: string, i: number): boolean {
  let k = i;
  while (k > 0 && s[k - 1] !== "\n") {
    k--;
    if (s[k] !== " " && s[k] !== "\t" && s[k] !== "\r") return false;
  }
  return true;
}

function restOfLineBlank(s: string, i: number): boolean {
  for (let j = i; j < s.length && s[j] !== "\n"; j++) if (!isSpace(s[j])) return false;
  return true;
}

function scriptLine(d: LangDialect, s: string, i: number): [TokenKind, number] | null {
  if (!lineStartBefore(s, i)) return null;
  if (d === "oracle" && s[i] === "/" && restOfLineBlank(s, i + 1)) return ["slashLine", i + 1];
  if (d === "msSql" && s.slice(i, i + 2).toUpperCase() === "GO") {
    let j = i + 2;
    const next = at(s, j);
    if (next && (isAlpha(next) || next === "_")) return null;
    if (isDigit(next)) return null;
    let k = j;
    while (k < s.length && (s[k] === " " || s[k] === "\t")) k++;
    if (k > j && isDigit(at(s, k))) {
      while (k < s.length && isDigit(s[k])) k++;
      j = k;
    }
    return restOfLineBlank(s, j) ? ["batchSeparator", j] : null;
  }
  if (isMysql(d) && s.length - i > 10 && s.slice(i, i + 9).toUpperCase() === "DELIMITER" && (s[i + 9] === " " || s[i + 9] === "\t")) {
    let end = lineEnd(s, i);
    while (end > i && isSpace(s[end - 1])) end--;
    return ["clientCommand", end];
  }
  return null;
}

function prevIsIdent(s: string, i: number): boolean {
  if (i === 0) return false;
  const p = s[i - 1];
  return isAlnum(p) || p === "_" || p === "$" || isHigh(p);
}

function skipQuoted(s: string, i: number, quote: string, backslash: boolean): number {
  while (i < s.length) {
    if (backslash && s[i] === "\\") {
      i += 2;
      continue;
    }
    if (s[i] === quote) {
      if (at(s, i + 1) === quote) {
        i += 2;
        continue;
      }
      return i + 1;
    }
    i++;
  }
  return s.length;
}

function prefixedString(d: LangDialect, s: string, i: number): number | null {
  if (prevIsIdent(s, i)) return null;
  const c = s[i].toUpperCase();
  if (d === "oracle") {
    let q = -1;
    if (c === "Q" && at(s, i + 1) === "'") q = i + 1;
    else if (c === "N" && at(s, i + 1).toUpperCase() === "Q" && at(s, i + 2) === "'") q = i + 2;
    if (q >= 0 && q + 1 < s.length) {
      const open = s[q + 1];
      const close = open === "[" ? "]" : open === "{" ? "}" : open === "(" ? ")" : open === "<" ? ">" : open;
      let j = q + 2;
      while (j < s.length && !(s[j] === close && at(s, j + 1) === "'")) j++;
      return Math.min(j + 2, s.length);
    }
  }
  if (d === "postgres" && c === "U" && at(s, i + 1) === "&" && at(s, i + 2) === "'") return skipQuoted(s, i + 3, "'", false);
  let j = i;
  while (j < s.length && (isAlnum(s[j]) || s[j] === "_")) j++;
  if (j >= s.length || s[j] !== "'" || j === i) return null;
  const word = s.slice(i, j);
  const upper = word.toUpperCase();
  let backslash: boolean;
  if (upper === "N" || upper === "X" || upper === "B") backslash = isMysql(d) || d === "clickHouse";
  else if (upper === "E" && d === "postgres") backslash = true;
  else if (isMysql(d) && word[0] === "_" && word.length > 1) backslash = true;
  else return null;
  return skipQuoted(s, j + 1, "'", backslash);
}

function identEnd(s: string, j: number): number {
  while (j < s.length && (isAlnum(s[j]) || s[j] === "_" || s[j] === "$" || isHigh(s[j]))) j++;
  return j;
}

function paramOrVariable(d: LangDialect, s: string, i: number): [TokenKind, number] | null {
  const c = s[i];
  const next = at(s, i + 1);
  if (c === "?" && d !== "postgres") {
    let j = i + 1;
    while (j < s.length && isDigit(s[j])) j++;
    return ["param", j];
  }
  if (
    c === ":" &&
    d !== "msSql" &&
    !isMysql(d) &&
    (isAlnum(next) || next === "_") &&
    (i === 0 || s[i - 1] !== ":") &&
    !(d === "postgres" && i > 0 && (isAlnum(s[i - 1]) || s[i - 1] === "]"))
  ) {
    return ["param", identEnd(s, i + 1)];
  }
  if (c === "$" && (d === "postgres" || d === "sqlite") && isDigit(next)) {
    let j = i + 1;
    while (j < s.length && isDigit(s[j])) j++;
    return ["param", j];
  }
  if ((c === "$" || c === "@") && d === "sqlite" && (isAlpha(next) || next === "_")) return ["param", identEnd(s, i + 1)];
  if (c === "@" && (d === "msSql" || isMysql(d))) {
    let j = i + 1;
    if (at(s, j) === "@") j++;
    if (isMysql(d) && (at(s, j) === "'" || at(s, j) === '"' || at(s, j) === "`")) return ["variable", skipQuoted(s, j + 1, s[j], true)];
    let k = j;
    while (k < s.length && (isAlnum(s[k]) || s[k] === "_" || s[k] === "$" || isHigh(s[k]) || ((s[k] === "#" || s[k] === "@") && d === "msSql"))) k++;
    return k > j ? ["variable", k] : null;
  }
  if (c === "{" && d === "clickHouse") {
    let j = i + 1;
    while (j < s.length && (s[j] === " " || s[j] === "\t")) j++;
    const nameStart = j;
    j = identEnd(s, j);
    if (j === nameStart) return null;
    while (j < s.length && (s[j] === " " || s[j] === "\t")) j++;
    if (at(s, j) !== ":") return null;
    let depth = 0;
    for (; j < s.length && s[j] !== "\n"; j++) {
      if (s[j] === "(") depth++;
      else if (s[j] === ")") depth--;
      else if (s[j] === "}" && depth === 0) return ["param", j + 1];
    }
    return null;
  }
  return null;
}

function isWordStart(d: LangDialect, c: string): boolean {
  return isAlpha(c) || c === "_" || isHigh(c) || ((c === "#" || c === "$") && d === "msSql");
}

function wordEnd(d: LangDialect, s: string, i: number): number {
  let j = i;
  while (j < s.length) {
    const c = s[j];
    const ok =
      isAlnum(c) ||
      c === "_" ||
      isHigh(c) ||
      (c === "$" && d !== "clickHouse") ||
      (c === "#" && (d === "oracle" || d === "msSql")) ||
      (c === "@" && d === "msSql");
    if (!ok) break;
    j++;
  }
  return j;
}

function operatorEnd(d: LangDialect, s: string, i: number): number {
  let j = i;
  while (j < s.length && OPERATOR_CHARS.includes(s[j])) {
    const c = s[j];
    if (j > i) {
      const next = at(s, j + 1);
      if (
        (c === "/" && next === "*") ||
        lineComment(d, s, j) !== null ||
        (c === "?" && d !== "postgres") ||
        (c === "@" && (d === "msSql" || isMysql(d) || d === "sqlite")) ||
        c === "$" ||
        isWordStart(d, c) ||
        (c === "`" && (isMysql(d) || d === "sqlite" || d === "clickHouse")) ||
        (c === ":" && s[j - 1] !== ":" && (isAlpha(next) || next === "_"))
      ) {
        break;
      }
    }
    j++;
  }
  const run = s.slice(i, j);
  if (run.length > 1 && ![...run].some((ch) => KEEPS_TRAILING_SIGN.includes(ch))) {
    let k = j;
    while (k > i + 1 && (s[k - 1] === "+" || s[k - 1] === "-")) k--;
    return k;
  }
  return j;
}

function dollarQuote(s: string, i: number): number | null {
  let j = i + 1;
  if (isDigit(at(s, j))) return null;
  while (j < s.length && (isAlnum(s[j]) || s[j] === "_" || isHigh(s[j]))) j++;
  if (j >= s.length || s[j] !== "$") return null;
  const tag = s.slice(i, j + 1);
  const close = s.indexOf(tag, j + 1);
  return close < 0 ? s.length : close + tag.length;
}

function numberEnd(s: string, i: number): number {
  let j = i;
  const hex = s[i] === "0" && (at(s, i + 1) === "x" || at(s, i + 1) === "X");
  while (j < s.length) {
    const c = s[j];
    if (isAlnum(c) || c === "_") {
      j++;
      if ((c === "e" || c === "E") && (at(s, j) === "+" || at(s, j) === "-") && isDigit(at(s, j + 1)) && !hex) j += 2;
    } else if (c === ".") {
      if (at(s, j + 1) === ".") break;
      j++;
    } else break;
  }
  return j;
}
