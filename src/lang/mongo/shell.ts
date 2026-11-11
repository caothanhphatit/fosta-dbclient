/**
 * mongosh-style query text to Extended JSON, so the backend only ever sees JSON.
 *
 * Accepted: JSON plus what people paste from mongosh, Compass and driver docs: unquoted keys, single-quoted strings,
 * trailing commas, `//` and `/* *\/` comments, regex literals (`/^a/i`) and the shell helpers `ObjectId("…")`,
 * `ISODate("…")`, `new Date("…")` / `new Date(ms)`, `NumberLong`, `NumberInt`, `NumberDecimal` / `Decimal128`, `UUID("…")`,
 * `BinData(subtype, "base64")`, `Timestamp(t, i)` / `Timestamp({ t, i })`, `RegExp("…", "flags")`, `MinKey` / `MaxKey`
 * (each optionally with `new`). Numbers follow mongosh: an integer that fits int32 is an int32, anything else a double.
 *
 * Output: Extended JSON v2 with every BSON-specific value in its canonical wrapper (`{"$oid":…}`,
 * `{"$date":{"$numberLong":…}}`, `{"$numberLong":…}`, `{"$binary":{…}}`, `{"$regularExpression":{…}}`, …), and
 * int32s, fractional doubles, strings, booleans and null as plain JSON. The backend parses it with serde_json and
 * bson's `Bson::try_from` (values.rs `parse_document`), which reads both flavours; the canonical wrappers keep the type
 * exact where the relaxed form would not (a relaxed `NumberLong(5)` is `5`, read back as an int32; a relaxed date is
 * an ISO string only between years 1970 and 9999). Key order is kept as written.
 *
 * Refused, with the position of the offending text: server-side JavaScript (functions, arrows, `Code`, `$where`,
 * `$function`, `$accumulator`), and anything mongosh would read differently from what it looks like: `Date()` /
 * `ISODate()` / `ObjectId()` without arguments (the current time or a new id), `Date("…")` without `new` (a string),
 * `new Date("…")` with a time and no zone (local time), integers past 2^53 (rounded), `NumberLong` / `NumberInt` out of
 * range, leading-zero integers (legacy octal), `undefined`, duplicate keys and regex flags a query cannot use.
 */

export type ShellParseResult = { ok: true; json: string } | { ok: false; error: string; position: number };

export interface ShellParseOptions {
  /** What the top-level value must be: a document (a filter, a projection), an array (a pipeline) or anything. */
  expect?: "document" | "array" | "any";
}

/** Parses mongosh-style `text` into an Extended JSON string; errors carry a 0-based offset into `text`. */
export function parseMongoShell(text: string, options: ShellParseOptions = {}): ShellParseResult {
  try {
    const parser = new Parser(text);
    const node = parser.parseTop();
    const expect = options.expect ?? "any";
    if (expect === "document" && node.k !== "obj") throw new ShellSyntaxError("Expected a document, e.g. { status: 'paid' }", node.pos);
    if (expect === "array" && node.k !== "arr") throw new ShellSyntaxError("Expected an array, e.g. [ { $match: {} } ]", node.pos);
    return { ok: true, json: render(node) };
  } catch (error) {
    if (error instanceof ShellSyntaxError) return { ok: false, error: error.message, position: error.position };
    throw error;
  }
}

/** The 1-based line and column of `position` in `text`, for showing a parse error. */
export function lineColumn(text: string, position: number): { line: number; column: number } {
  const before = text.slice(0, position);
  const lines = before.split("\n");
  return { line: lines.length, column: lines[lines.length - 1].length + 1 };
}

class ShellSyntaxError extends Error {
  constructor(
    message: string,
    readonly position: number,
  ) {
    super(message);
  }
}

type Node =
  | { k: "str"; v: string; pos: number }
  | { k: "num"; v: number; text: string; pos: number }
  | { k: "ejson"; json: string; pos: number }
  | { k: "obj"; entries: [string, Node][]; pos: number }
  | { k: "arr"; items: Node[]; pos: number };

const INT32_MIN = -2147483648;
const INT32_MAX = 2147483647;
const INT64_MIN = -(2n ** 63n);
const INT64_MAX = 2n ** 63n - 1n;
const UINT32_MAX = 4294967295;
const DENIED_KEYS = new Set(["$where", "$function", "$accumulator"]);
const SERVER_JS = "runs JavaScript on the server and is not allowed";

function render(node: Node): string {
  switch (node.k) {
    case "str":
      return JSON.stringify(node.v);
    case "num":
      return renderNumber(node.v);
    case "ejson":
      return node.json;
    case "obj":
      return `{${node.entries.map(([key, value]) => `${JSON.stringify(key)}:${render(value)}`).join(",")}}`;
    case "arr":
      return `[${node.items.map(render).join(",")}]`;
  }
}

function renderNumber(v: number): string {
  if (Number.isNaN(v)) return '{"$numberDouble":"NaN"}';
  if (v === Infinity) return '{"$numberDouble":"Infinity"}';
  if (v === -Infinity) return '{"$numberDouble":"-Infinity"}';
  if (Number.isInteger(v)) {
    if (v >= INT32_MIN && v <= INT32_MAX) return String(v === 0 ? 0 : v);
    // Plain JSON would read back as an int64; mongosh stores it as a double.
    return `{"$numberDouble":${JSON.stringify(String(v))}}`;
  }
  return String(v); // has a "." or an exponent, so it reads back as a double
}

const ejson = (json: string, pos: number): Node => ({ k: "ejson", json, pos });

const isIdStart = (ch: string) => /[\p{ID_Start}$_]/u.test(ch);
const isIdPart = (ch: string) => /[\p{ID_Continue}$‌‍]/u.test(ch);
const isDigit = (ch: string | undefined) => ch !== undefined && ch >= "0" && ch <= "9";

class Parser {
  private i = 0;

  constructor(private readonly s: string) {}

  parseTop(): Node {
    this.ws();
    if (this.i >= this.s.length) throw new ShellSyntaxError("Nothing to parse", this.i);
    const node = this.value();
    this.ws();
    if (this.s[this.i] === ";") {
      this.i++;
      this.ws();
    }
    if (this.i < this.s.length) throw new ShellSyntaxError("Unexpected text after the value", this.i);
    return node;
  }

  // --- Whitespace and comments --------------------------------------------------------------------------------------

  private ws(): void {
    const s = this.s;
    while (this.i < s.length) {
      const ch = s[this.i];
      if (ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === "﻿" || /\s/u.test(ch)) {
        this.i++;
      } else if (ch === "/" && s[this.i + 1] === "/") {
        while (this.i < s.length && s[this.i] !== "\n") this.i++;
      } else if (ch === "/" && s[this.i + 1] === "*") {
        const end = s.indexOf("*/", this.i + 2);
        if (end < 0) throw new ShellSyntaxError("Unterminated comment", this.i);
        this.i = end + 2;
      } else {
        return;
      }
    }
  }

  private expect(ch: string, what: string): void {
    this.ws();
    if (this.s[this.i] !== ch) throw new ShellSyntaxError(this.i >= this.s.length ? `Unexpected end of input, expected ${what}` : `Expected ${what}`, this.i);
    this.i++;
  }

  // --- Values -------------------------------------------------------------------------------------------------------

  private value(): Node {
    this.ws();
    const s = this.s;
    const pos = this.i;
    const ch = s[pos];
    if (ch === undefined) throw new ShellSyntaxError("Unexpected end of input, expected a value", pos);
    if (ch === "{") return this.object();
    if (ch === "[") return this.array();
    if (ch === '"' || ch === "'") return { k: "str", v: this.string(), pos };
    if (ch === "`") throw new ShellSyntaxError("Template literals are not supported; use '…' or \"…\"", pos);
    if (ch === "/") return this.regexLiteral();
    if (ch === "(") {
      if (this.arrowAfterParens(pos)) throw new ShellSyntaxError(`A function ${SERVER_JS}`, pos);
      throw new ShellSyntaxError("Parenthesized expressions are not supported", pos);
    }
    if (ch === "-" || ch === "+" || isDigit(ch) || (ch === "." && isDigit(s[pos + 1]))) return this.number();
    if (isIdStart(this.codePoint())) return this.identifierValue();
    throw new ShellSyntaxError(`Unexpected character ${JSON.stringify(ch)}`, pos);
  }

  private codePoint(at = this.i): string {
    const cp = this.s.codePointAt(at);
    return cp === undefined ? "" : String.fromCodePoint(cp);
  }

  private identifier(): string {
    const start = this.i;
    let cp = this.codePoint();
    if (!cp || !isIdStart(cp)) throw new ShellSyntaxError("Expected a name", start);
    this.i += cp.length;
    while (this.i < this.s.length && (cp = this.codePoint()) && isIdPart(cp)) this.i += cp.length;
    return this.s.slice(start, this.i);
  }

  /** Whether `(` at `pos` closes and is followed by `=>` (an arrow function). */
  private arrowAfterParens(pos: number): boolean {
    let depth = 0;
    for (let j = pos; j < this.s.length; j++) {
      if (this.s[j] === "(") depth++;
      else if (this.s[j] === ")" && --depth === 0) return /^\s*=>/.test(this.s.slice(j + 1));
    }
    return false;
  }

  private object(): Node {
    const pos = this.i;
    this.i++; // {
    const entries: [string, Node][] = [];
    const seen = new Set<string>();
    for (;;) {
      this.ws();
      if (this.s[this.i] === "}") {
        this.i++;
        return { k: "obj", entries, pos };
      }
      const keyPos = this.i;
      const key = this.key();
      if (DENIED_KEYS.has(key)) throw new ShellSyntaxError(`${key} ${SERVER_JS}`, keyPos);
      if (seen.has(key)) throw new ShellSyntaxError(`Duplicate key ${JSON.stringify(key)}: mongosh would keep only the last one`, keyPos);
      seen.add(key);
      this.ws();
      if (this.s[this.i] === "(") throw new ShellSyntaxError(`A method ${SERVER_JS}`, keyPos);
      if (this.s[this.i] === "," || this.s[this.i] === "}") throw new ShellSyntaxError(`Key ${JSON.stringify(key)} needs a value, e.g. ${key}: 1`, this.i);
      this.expect(":", "':' after the key");
      entries.push([key, this.value()]);
      this.ws();
      const next = this.s[this.i];
      if (next === ",") {
        this.i++;
        this.ws();
        if (this.s[this.i] === ",") throw new ShellSyntaxError("Expected a key", this.i);
      } else if (next !== "}") {
        throw new ShellSyntaxError(next === undefined ? "Unexpected end of input, expected '}'" : "Expected ',' or '}'", this.i);
      }
    }
  }

  private key(): string {
    const s = this.s;
    const pos = this.i;
    const ch = s[pos];
    if (ch === undefined) throw new ShellSyntaxError("Unexpected end of input, expected '}'", pos);
    if (ch === '"' || ch === "'") return this.string();
    if (isDigit(ch) || (ch === "." && isDigit(s[pos + 1]))) {
      const node = this.number();
      return String((node as { v: number }).v);
    }
    if (ch === "[") throw new ShellSyntaxError("Computed keys are not supported; quote the key", pos);
    if (ch === "." && s.startsWith("...", pos)) throw new ShellSyntaxError("Spread syntax is not supported", pos);
    if (!isIdStart(this.codePoint())) throw new ShellSyntaxError("Expected a key", pos);
    const name = this.identifier();
    if (s[this.i] === "." || s[this.i] === "-") {
      throw new ShellSyntaxError(`Quote keys that contain '${s[this.i]}', e.g. "${name}${s[this.i]}…"`, pos);
    }
    return name;
  }

  private array(): Node {
    const pos = this.i;
    this.i++; // [
    const items: Node[] = [];
    for (;;) {
      this.ws();
      if (this.s[this.i] === "]") {
        this.i++;
        return { k: "arr", items, pos };
      }
      if (this.s[this.i] === ",") throw new ShellSyntaxError("Empty array element", this.i);
      items.push(this.value());
      this.ws();
      const next = this.s[this.i];
      if (next === ",") this.i++;
      else if (next !== "]") throw new ShellSyntaxError(next === undefined ? "Unexpected end of input, expected ']'" : "Expected ',' or ']'", this.i);
    }
  }

  private string(): string {
    const s = this.s;
    const start = this.i;
    const quote = s[this.i++];
    let out = "";
    for (;;) {
      const ch = s[this.i];
      if (ch === undefined || ch === "\n" || ch === "\r") throw new ShellSyntaxError("Unterminated string", start);
      this.i++;
      if (ch === quote) return out;
      if (ch !== "\\") {
        out += ch;
        continue;
      }
      const escPos = this.i - 1;
      const e = s[this.i++];
      switch (e) {
        case "n":
          out += "\n";
          break;
        case "t":
          out += "\t";
          break;
        case "r":
          out += "\r";
          break;
        case "b":
          out += "\b";
          break;
        case "f":
          out += "\f";
          break;
        case "v":
          out += "\v";
          break;
        case "0":
          if (isDigit(s[this.i])) throw new ShellSyntaxError("Octal escapes are not supported", escPos);
          out += "\0";
          break;
        case "x":
          out += String.fromCharCode(this.hexDigits(2, escPos));
          break;
        case "u":
          if (s[this.i] === "{") {
            const close = s.indexOf("}", this.i);
            const hex = close < 0 ? "" : s.slice(this.i + 1, close);
            if (!/^[0-9a-fA-F]{1,6}$/.test(hex) || parseInt(hex, 16) > 0x10ffff) throw new ShellSyntaxError("Invalid \\u{…} escape", escPos);
            out += String.fromCodePoint(parseInt(hex, 16));
            this.i = close + 1;
          } else {
            out += String.fromCharCode(this.hexDigits(4, escPos));
          }
          break;
        case "\r":
          if (s[this.i] === "\n") this.i++;
          break;
        case "\n":
        case " ":
        case " ":
          break; // line continuation
        case undefined:
          throw new ShellSyntaxError("Unterminated string", start);
        default:
          if (isDigit(e)) throw new ShellSyntaxError("Octal escapes are not supported", escPos);
          out += e; // \' \" \\ \/ and identity escapes
      }
    }
  }

  private hexDigits(count: number, escPos: number): number {
    const hex = this.s.slice(this.i, this.i + count);
    if (hex.length !== count || !/^[0-9a-fA-F]+$/.test(hex)) throw new ShellSyntaxError("Invalid escape sequence", escPos);
    this.i += count;
    return parseInt(hex, 16);
  }

  private number(): Node {
    const s = this.s;
    const pos = this.i;
    let sign = 1;
    if (s[this.i] === "-" || s[this.i] === "+") {
      if (s[this.i] === "-") sign = -1;
      this.i++;
      if (s.startsWith("Infinity", this.i) && !isIdPart(this.codePoint(this.i + 8))) {
        this.i += 8;
        return { k: "num", v: sign * Infinity, text: s.slice(pos, this.i), pos };
      }
      if (!isDigit(s[this.i]) && !(s[this.i] === "." && isDigit(s[this.i + 1]))) throw new ShellSyntaxError(`Expected a number after '${s[pos]}'`, pos);
    }
    const bodyStart = this.i;
    let value: number;
    let integerLiteral: boolean;
    const radix = s[this.i] === "0" ? { x: 16, X: 16, o: 8, O: 8, b: 2, B: 2 }[s[this.i + 1] ?? ""] : undefined;
    if (radix) {
      this.i += 2;
      const digitsStart = this.i;
      const valid = radix === 16 ? /[0-9a-fA-F]/ : radix === 8 ? /[0-7]/ : /[01]/;
      while (this.i < s.length && valid.test(s[this.i])) this.i++;
      if (this.i === digitsStart) throw new ShellSyntaxError("Expected digits after the number prefix", pos);
      const big = BigInt(s.slice(bodyStart, this.i));
      value = Number(big);
      integerLiteral = true;
      if (big > BigInt(Number.MAX_SAFE_INTEGER)) throw this.unsafeInteger(pos);
    } else {
      while (isDigit(s[this.i])) this.i++;
      const intDigits = s.slice(bodyStart, this.i);
      if (intDigits.length > 1 && intDigits[0] === "0") throw new ShellSyntaxError("Integers with leading zeros are ambiguous (legacy octal in mongosh)", pos);
      integerLiteral = true;
      if (s[this.i] === ".") {
        integerLiteral = false;
        this.i++;
        while (isDigit(s[this.i])) this.i++;
      }
      if (s[this.i] === "e" || s[this.i] === "E") {
        integerLiteral = false;
        const ePos = this.i;
        this.i++;
        if (s[this.i] === "+" || s[this.i] === "-") this.i++;
        if (!isDigit(s[this.i])) throw new ShellSyntaxError("Expected digits in the exponent", ePos);
        while (isDigit(s[this.i])) this.i++;
      }
      value = Number(s.slice(bodyStart, this.i));
      if (integerLiteral && !Number.isSafeInteger(value)) throw this.unsafeInteger(pos);
    }
    if (this.i < s.length && (isIdPart(this.codePoint()) || s[this.i] === ".")) throw new ShellSyntaxError("Invalid number", pos);
    return { k: "num", v: sign * value, text: s.slice(pos, this.i), pos };
  }

  private unsafeInteger(pos: number): ShellSyntaxError {
    return new ShellSyntaxError('Integer is beyond 2^53 and mongosh would round it; use NumberLong("…") or NumberDecimal("…")', pos);
  }

  private regexLiteral(): Node {
    const s = this.s;
    const pos = this.i;
    this.i++; // /
    let inClass = false;
    for (;;) {
      const ch = s[this.i];
      if (ch === undefined || ch === "\n" || ch === "\r") throw new ShellSyntaxError("Unterminated regular expression", pos);
      this.i++;
      if (ch === "\\") {
        if (s[this.i] === undefined || s[this.i] === "\n") throw new ShellSyntaxError("Unterminated regular expression", pos);
        this.i++;
      } else if (ch === "[") inClass = true;
      else if (ch === "]") inClass = false;
      else if (ch === "/" && !inClass) break;
    }
    const pattern = s.slice(pos + 1, this.i - 1);
    const flagsPos = this.i;
    while (this.i < s.length && /[A-Za-z0-9_$]/.test(s[this.i])) this.i++;
    const flags = s.slice(flagsPos, this.i);
    return ejson(regexJson(pattern, flags, flagsPos, "imsu"), pos);
  }

  // --- Names: literals and shell helpers ----------------------------------------------------------------------------

  private identifierValue(): Node {
    const pos = this.i;
    let name = this.identifier();
    let withNew = false;
    if (name === "new") {
      this.ws();
      if (!isIdStart(this.codePoint())) throw new ShellSyntaxError("Expected a type name after new", this.i);
      withNew = true;
      name = this.identifier();
    }
    const namePos = withNew ? this.i - name.length : pos;
    this.ws();
    if (this.s.startsWith("=>", this.i)) throw new ShellSyntaxError(`A function ${SERVER_JS}`, pos);
    if (name === "function" || name === "async") throw new ShellSyntaxError(`A function ${SERVER_JS}`, pos);
    if (this.s[this.i] === "(") {
      const args = this.args();
      return this.call(name, withNew, args, pos, namePos);
    }
    if (withNew) throw new ShellSyntaxError(`Expected '(' after new ${name}`, this.i);
    if (this.s[this.i] === "." || this.s[this.i] === "[") throw new ShellSyntaxError("Property access is not supported", this.i);
    switch (name) {
      case "true":
      case "false":
      case "null":
        return ejson(name, pos);
      case "NaN":
        return { k: "num", v: NaN, text: name, pos };
      case "Infinity":
        return { k: "num", v: Infinity, text: name, pos };
      case "MinKey":
        return ejson('{"$minKey":1}', pos);
      case "MaxKey":
        return ejson('{"$maxKey":1}', pos);
      case "undefined":
        throw new ShellSyntaxError("undefined is ambiguous (mongosh may drop the field or store null); use null", pos);
      default:
        if (HELPERS.has(name)) throw new ShellSyntaxError(`Expected '(' after ${name}`, this.i);
        throw new ShellSyntaxError(`Unknown name ${JSON.stringify(name)}; quote strings, e.g. '${name}'`, pos);
    }
  }

  private args(): Node[] {
    this.i++; // (
    const args: Node[] = [];
    for (;;) {
      this.ws();
      if (this.s[this.i] === ")") {
        this.i++;
        return args;
      }
      args.push(this.value());
      this.ws();
      const next = this.s[this.i];
      if (next === ",") this.i++;
      else if (next !== ")") throw new ShellSyntaxError(next === undefined ? "Unexpected end of input, expected ')'" : "Expected ',' or ')'", this.i);
    }
  }

  private call(name: string, withNew: boolean, args: Node[], pos: number, namePos: number): Node {
    const arity = (min: number, max: number, usage: string) => {
      if (args.length < min || args.length > max) {
        const at = args.length > max ? args[max].pos : namePos;
        throw new ShellSyntaxError(`${name} takes ${usage}`, at);
      }
    };
    const str = (index: number, what: string): string => {
      const arg = args[index];
      if (arg.k !== "str") throw new ShellSyntaxError(`${name} needs ${what}`, arg.pos);
      return arg.v;
    };
    switch (name) {
      case "ObjectId": {
        if (args.length === 0) throw new ShellSyntaxError('ObjectId() makes a new id; give the id, e.g. ObjectId("65a1f0c2e4b0a1b2c3d4e5f6")', namePos);
        arity(1, 1, "one 24-digit hex string");
        const hex = str(0, "a 24-digit hex string");
        if (!/^[0-9a-fA-F]{24}$/.test(hex)) throw new ShellSyntaxError("ObjectId needs 24 hex digits", args[0].pos);
        return ejson(`{"$oid":"${hex.toLowerCase()}"}`, pos);
      }
      case "ISODate":
      case "Date": {
        if (name === "Date" && !withNew)
          throw new ShellSyntaxError('Date(…) without new returns a string in mongosh; use new Date("…") or ISODate("…")', namePos);
        if (args.length === 0)
          throw new ShellSyntaxError(`${withNew ? "new " : ""}${name}() is the current time; give the date, e.g. ISODate("2024-01-31T00:00:00Z")`, namePos);
        if (args.length > 1) {
          if (name === "Date") throw new ShellSyntaxError('new Date(year, month, …) is in local time; use ISODate("…") with a zone', namePos);
          arity(1, 1, "one ISO date string");
        }
        const arg = args[0];
        if (arg.k === "num" && name === "Date") {
          if (!Number.isInteger(arg.v)) throw new ShellSyntaxError("new Date(ms) needs whole milliseconds", arg.pos);
          if (Math.abs(arg.v) > 8.64e15) throw new ShellSyntaxError("Date is out of range", arg.pos);
          return ejson(dateJson(arg.v), pos);
        }
        const text = str(0, name === "Date" ? "an ISO date string or milliseconds" : "an ISO date string");
        return ejson(dateJson(parseIsoDate(text, name === "ISODate", arg.pos)), pos);
      }
      case "NumberLong": {
        arity(1, 1, 'one integer, e.g. NumberLong("9007199254740993")');
        const arg = args[0];
        let big: bigint;
        if (arg.k === "str") {
          if (!/^[+-]?\d+$/.test(arg.v.trim())) throw new ShellSyntaxError("NumberLong needs an integer", arg.pos);
          big = BigInt(arg.v.trim());
        } else if (arg.k === "num" && Number.isInteger(arg.v)) {
          if (!Number.isSafeInteger(arg.v))
            throw new ShellSyntaxError('NumberLong of a number beyond 2^53 is rounded; pass it as a string, e.g. NumberLong("…")', arg.pos);
          big = BigInt(arg.v);
        } else {
          throw new ShellSyntaxError("NumberLong needs an integer", arg.pos);
        }
        if (big < INT64_MIN || big > INT64_MAX) throw new ShellSyntaxError("NumberLong is out of the 64-bit range", arg.pos);
        return ejson(`{"$numberLong":"${big.toString()}"}`, pos);
      }
      case "NumberInt": {
        arity(1, 1, "one integer");
        const arg = args[0];
        let v: number;
        if (arg.k === "str" && /^[+-]?\d+$/.test(arg.v.trim())) v = Number(arg.v.trim());
        else if (arg.k === "num" && Number.isInteger(arg.v)) v = arg.v;
        else throw new ShellSyntaxError("NumberInt needs an integer", arg.pos);
        if (v < INT32_MIN || v > INT32_MAX) throw new ShellSyntaxError("NumberInt is out of the 32-bit range", arg.pos);
        return ejson(`{"$numberInt":"${v === 0 ? 0 : v}"}`, pos);
      }
      case "NumberDecimal":
      case "Decimal128": {
        arity(1, 1, 'one decimal string, e.g. NumberDecimal("19.99")');
        const arg = args[0];
        let text: string;
        if (arg.k === "str") text = arg.v.trim();
        else if (arg.k === "num" && Number.isFinite(arg.v)) text = /^[+-]?0[xXoObB]/.test(arg.text) ? String(arg.v) : arg.text.replace(/^\+/, "");
        else throw new ShellSyntaxError(`${name} needs a decimal string`, arg.pos);
        const problem = decimalProblem(text);
        if (problem) throw new ShellSyntaxError(`${name}: ${problem}`, arg.pos);
        return ejson(`{"$numberDecimal":${JSON.stringify(text)}}`, pos);
      }
      case "UUID": {
        if (args.length === 0)
          throw new ShellSyntaxError('UUID() makes a new random UUID; give it, e.g. UUID("123e4567-e89b-12d3-a456-426614174000")', namePos);
        arity(1, 1, "one UUID string");
        const text = str(0, "a UUID string");
        if (!/^([0-9a-fA-F]{32}|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/.test(text)) {
          throw new ShellSyntaxError("UUID needs 32 hex digits (dashes optional)", args[0].pos);
        }
        return ejson(binaryJson(hexToBase64(text.replace(/-/g, "")), 4), pos);
      }
      case "BinData": {
        arity(2, 2, 'a subtype and a base64 string, e.g. BinData(0, "AQID")');
        const sub = args[0];
        if (sub.k !== "num" || !Number.isInteger(sub.v) || sub.v < 0 || sub.v > 255)
          throw new ShellSyntaxError("BinData subtype must be an integer from 0 to 255", sub.pos);
        const data = str(1, "a base64 string");
        if (!/^[A-Za-z0-9+/]*={0,2}$/.test(data) || data.length % 4 !== 0) throw new ShellSyntaxError("BinData needs valid base64", args[1].pos);
        return ejson(binaryJson(data, sub.v), pos);
      }
      case "Timestamp": {
        let t: Node | undefined;
        let inc: Node | undefined;
        if (args.length === 1 && args[0].k === "obj") {
          const entries = new Map(args[0].entries);
          const extra = args[0].entries.find(([key]) => key !== "t" && key !== "i");
          if (extra) throw new ShellSyntaxError("Timestamp({ t, i }) takes only t and i", args[0].pos);
          t = entries.get("t");
          inc = entries.get("i");
          if (!t || !inc) throw new ShellSyntaxError("Timestamp({ t, i }) needs both t and i", args[0].pos);
        } else {
          arity(2, 2, "seconds and an increment, e.g. Timestamp({ t: 1700000000, i: 1 })");
          [t, inc] = args;
        }
        for (const part of [t, inc]) {
          if (part.k !== "num" || !Number.isInteger(part.v) || part.v < 0 || part.v > UINT32_MAX) {
            throw new ShellSyntaxError("Timestamp parts must be integers from 0 to 4294967295", part.pos);
          }
        }
        return ejson(`{"$timestamp":{"t":${(t as { v: number }).v},"i":${(inc as { v: number }).v}}}`, pos);
      }
      case "RegExp": {
        arity(1, 2, 'a pattern and optional flags, e.g. RegExp("^a", "i")');
        const pattern = str(0, "a pattern string");
        const flags = args.length > 1 ? str(1, "a flags string") : "";
        return ejson(regexJson(pattern, flags, args[1]?.pos ?? namePos, "imsux"), pos);
      }
      case "MinKey":
      case "MaxKey":
        arity(0, 0, "no arguments");
        return ejson(name === "MinKey" ? '{"$minKey":1}' : '{"$maxKey":1}', pos);
      case "Code":
        throw new ShellSyntaxError(`Code ${SERVER_JS}`, namePos);
      default:
        throw new ShellSyntaxError(`Unknown function ${name}`, namePos);
    }
  }
}

const HELPERS = new Set([
  "ObjectId",
  "ISODate",
  "Date",
  "NumberLong",
  "NumberInt",
  "NumberDecimal",
  "Decimal128",
  "UUID",
  "BinData",
  "Timestamp",
  "RegExp",
  "Code",
]);

function regexJson(pattern: string, flags: string, flagsPos: number, allowed: string): string {
  const seen = new Set<string>();
  for (const flag of flags) {
    if (!allowed.includes(flag)) {
      const why = flag === "g" || flag === "y" || flag === "d" ? "has no meaning in a query" : "is not a MongoDB regex option";
      throw new ShellSyntaxError(`Regex flag '${flag}' ${why}`, flagsPos);
    }
    if (seen.has(flag)) throw new ShellSyntaxError(`Regex flag '${flag}' is repeated`, flagsPos);
    seen.add(flag);
  }
  const options = [...seen].sort().join("");
  return `{"$regularExpression":{"pattern":${JSON.stringify(pattern)},"options":"${options}"}}`;
}

function dateJson(ms: number): string {
  return `{"$date":{"$numberLong":"${ms === 0 ? 0 : ms}"}}`;
}

function binaryJson(base64: string, subtype: number): string {
  return `{"$binary":{"base64":"${base64}","subType":"${subtype.toString(16).padStart(2, "0")}"}}`;
}

function hexToBase64(hex: string): string {
  let bytes = "";
  for (let j = 0; j < hex.length; j += 2) bytes += String.fromCharCode(parseInt(hex.slice(j, j + 2), 16));
  return btoa(bytes);
}

/**
 * Milliseconds since the epoch of an ISO 8601 date. A time without a zone is UTC for `ISODate` (as in mongosh) and
 * refused for `new Date` (JavaScript reads it as local time). Digits past milliseconds are dropped, as BSON dates keep ms.
 */
function parseIsoDate(text: string, isoDate: boolean, pos: number): number {
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d+))?)?)?\s*(Z|[+-]\d{2}(?::?\d{2})?)?$/i.exec(text.trim());
  if (!m) throw new ShellSyntaxError('Expected an ISO date, e.g. "2024-01-31" or "2024-01-31T12:00:00Z"', pos);
  const [, y, mo, d, hh, mi, ss, frac, zone] = m;
  if (hh !== undefined && zone === undefined && !isoDate) {
    throw new ShellSyntaxError('new Date with a time and no zone is local time in mongosh; add Z or an offset, e.g. "2024-01-31T12:00:00Z"', pos);
  }
  if (hh === undefined && zone !== undefined) throw new ShellSyntaxError("A zone needs a time, e.g. 2024-01-31T00:00:00Z", pos);
  const year = Number(y);
  const month = Number(mo);
  const day = Number(d);
  const hour = Number(hh ?? 0);
  const minute = Number(mi ?? 0);
  const second = Number(ss ?? 0);
  const ms = frac ? Number(frac.slice(0, 3).padEnd(3, "0")) : 0;
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  date.setUTCHours(hour, minute, second, ms);
  if (month < 1 || month > 12 || date.getUTCDate() !== day || date.getUTCMonth() !== month - 1 || hour > 23 || minute > 59 || second > 59) {
    throw new ShellSyntaxError(`Invalid date ${JSON.stringify(text)}`, pos);
  }
  let offset = 0;
  if (zone && zone.toUpperCase() !== "Z") {
    const digits = zone.slice(1).replace(":", "");
    const oh = Number(digits.slice(0, 2));
    const om = Number(digits.slice(2) || 0);
    if (oh > 23 || om > 59) throw new ShellSyntaxError(`Invalid zone offset ${zone}`, pos);
    offset = (zone[0] === "-" ? -1 : 1) * (oh * 60 + om) * 60000;
  }
  return date.getTime() - offset;
}

/** Why `text` is not an exact Decimal128, or undefined when it is. */
function decimalProblem(text: string): string | undefined {
  if (/^[+-]?(NaN|Inf|Infinity)$/i.test(text)) return undefined;
  const m = /^[+-]?(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(text);
  if (!m || (m[1] === "" && (m[2] ?? "") === "")) return `${JSON.stringify(text)} is not a decimal number`;
  const digits = (m[1] + (m[2] ?? "")).replace(/^0+/, "");
  if (digits.length > 34) return "more than 34 significant digits would be rounded";
  const exponent = Number(m[3] ?? 0) - (m[2] ?? "").length;
  if (exponent < -6176 || exponent > 6111) return "the exponent is out of range";
  return undefined;
}
