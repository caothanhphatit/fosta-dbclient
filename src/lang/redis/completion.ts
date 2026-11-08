// Redis console completion: command names (and container subcommands such as CONFIG GET), then per command its
// literal option tokens (EX, NX, WITHSCORES...) and key names at key positions. One command per line, as redis-cli.
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { TextRange } from "../types";
import { findCommand, isKeyArgument, optionTokens, REDIS_COMMANDS, type RedisCommand } from "./commands";

interface Word {
  text: string;
  from: number;
  to: number;
}

/** Splits a line into words like redis-cli (whitespace separated, quotes group). Offsets are relative to the line. */
export function redisWords(line: string): Word[] {
  const out: Word[] = [];
  let i = 0;
  while (i < line.length) {
    while (i < line.length && /\s/.test(line[i])) i++;
    if (i >= line.length) break;
    const from = i;
    let text = "";
    while (i < line.length && !/\s/.test(line[i])) {
      const c = line[i];
      if (c === '"' || c === "'") {
        i++;
        while (i < line.length && line[i] !== c) {
          if (line[i] === "\\" && c === '"' && i + 1 < line.length) i++;
          text += line[i];
          i++;
        }
        i++;
      } else {
        text += c;
        i++;
      }
    }
    out.push({ text, from, to: Math.min(i, line.length) });
  }
  return out;
}

const signature = (c: RedisCommand) => `${c.name} ${c.args}`.trim();

/** Completions for `pos` in `doc`; `keys` lists known key names (exported for tests). */
export function redisCompletions(doc: string, pos: number, keys: string[] = [], explicit = false): CompletionResult | null {
  const lineStart = doc.lastIndexOf("\n", pos - 1) + 1;
  const line = doc.slice(lineStart, pos);
  const words = redisWords(line);
  const typingWord = line.length > 0 && !/\s$/.test(line);
  const current = typingWord ? words[words.length - 1] : undefined;
  const done = typingWord ? words.slice(0, -1) : words;
  const from = lineStart + (current ? current.from : line.length);
  const prefix = current?.text ?? "";
  if (!explicit && !typingWord) return null;
  if (current && /^["']/.test(line.slice(current.from)) && done.length === 0) return null;
  const lower = prefix !== "" && prefix === prefix.toLowerCase() && /[a-z]/.test(prefix);
  const cased = (s: string) => (lower ? s.toLowerCase() : s);

  if (done.length === 0) {
    const options: Completion[] = REDIS_COMMANDS.map((c) => ({
      label: cased(c.name),
      type: "keyword",
      detail: c.group,
      info: `${signature(c)}\n${c.summary}`,
    }));
    return { from, to: pos, options, validFor: /^[\w-]*$/ };
  }
  const top = findCommand([done[0].text]);
  if (top?.subcommands && done.length === 1) {
    const options = top.subcommands.map((s) => {
      const sub = s.name.slice(top.name.length + 1);
      return { label: cased(sub), type: "keyword", detail: top.name, info: `${signature(s)}\n${s.summary}` };
    });
    return { from, to: pos, options, validFor: /^[\w-]*$/ };
  }
  const cmd = findCommand(done.map((w) => w.text));
  if (!cmd) return null;
  const fixed = cmd.name.split(" ").length;
  const argIndex = done.length - fixed + 1;
  const used = new Set(done.slice(fixed).map((w) => w.text.toUpperCase()));
  const options: Completion[] = [];
  if (isKeyArgument(cmd, argIndex)) {
    for (const k of keys) options.push({ label: k, apply: quoteArg(k), type: "variable", detail: "key", boost: 1 });
  }
  for (const t of optionTokens(cmd.args)) {
    if (!used.has(t)) options.push({ label: cased(t), type: "keyword", detail: cmd.name, info: signature(cmd) });
  }
  return options.length ? { from, to: pos, options, validFor: /^[^\s"']*$/ } : null;
}

/** Quotes a key for redis-cli when it holds whitespace or quotes. */
export function quoteArg(value: string): string {
  if (value !== "" && !/[\s"'\\]/.test(value)) return value;
  return '"' + value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n") + '"';
}

/** `LanguageSupport.statementAt` for the Redis console: the current line (empty lines give an empty range). */
export function redisStatementAt(text: string, offset: number): TextRange {
  const from = text.lastIndexOf("\n", offset - 1) + 1;
  const nl = text.indexOf("\n", offset);
  let to = nl < 0 ? text.length : nl;
  if (to > from && text[to - 1] === "\r") to--;
  let start = from;
  while (start < to && /\s/.test(text[start])) start++;
  while (to > start && /\s/.test(text[to - 1])) to--;
  return { from: start, to };
}

/** A CodeMirror completion source; `keys` returns the key names known for the connection (e.g. from a SCAN). */
export function redisCompletionSource(keys: () => string[] = () => []): CompletionSource {
  return (context: CompletionContext) => redisCompletions(context.state.doc.toString(), context.pos, keys(), context.explicit);
}
