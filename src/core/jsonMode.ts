/**
 * A small CodeMirror stream mode for JSON and relaxed JSON (unquoted keys, `ObjectId("…")`), used by the JSON command
 * languages (MongoDB). Loaded on first use by `jsonLanguageSupport`.
 */
import { StreamLanguage, type StringStream } from "@codemirror/language";
import type { Extension } from "@codemirror/state";

function readString(stream: StringStream, quote: string): void {
  let escaped = false;
  let char: string | void;
  while ((char = stream.next()) != null) {
    if (char === quote && !escaped) return;
    escaped = !escaped && char === "\\";
  }
}

const json = StreamLanguage.define({
  name: "json",
  token(stream) {
    if (stream.eatSpace()) return null;
    const char = stream.peek();
    if (char === '"' || char === "'") {
      stream.next();
      readString(stream, char);
      return stream.match(/^\s*:/, false) ? "propertyName" : "string";
    }
    if (stream.match(/^-?\d+(\.\d+)?([eE][+-]?\d+)?/)) return "number";
    if (stream.match(/^(true|false)\b/)) return "bool";
    if (stream.match(/^null\b/)) return "null";
    if (stream.match(/^[\w$]+/)) return stream.match(/^\s*:/, false) ? "propertyName" : "typeName";
    if (stream.match(/^[{}[\]]/)) return "bracket";
    stream.next();
    return "punctuation";
  },
  languageData: { closeBrackets: { brackets: ["(", "[", "{", '"', "'"] } },
});

export function jsonMode(): Extension {
  return json;
}
