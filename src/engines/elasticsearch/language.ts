/**
 * The `es-console` editor language: Dev Tools highlighting (method, path, JSON body), completion of methods,
 * endpoints, index names (`es.names`), Query DSL keys and the request's index fields (`es.fields`), the request block
 * under the cursor for "Run current statement", and body formatting. Loaded lazily with the engine module.
 */
import type { CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import { StreamLanguage, type StringStream } from "@codemirror/language";
import { EditorState, type Extension } from "@codemirror/state";
import type { ConnectionApi } from "../../core/connectionApi";
import type { LanguageContext, LanguageSupport } from "../../core/language";
import { blocks, consoleCompletions, consoleStatementAt, formatConsole, indexOfPath, type ConsoleNames } from "./console";

interface ModeState {
  /** The rest of the line is the request path. */
  path: boolean;
}

function readString(stream: StringStream): void {
  let escaped = false;
  let char: string | void;
  while ((char = stream.next()) != null) {
    if (char === '"' && !escaped) return;
    escaped = !escaped && char === "\\";
  }
}

const consoleMode = StreamLanguage.define<ModeState>({
  name: "es-console",
  startState: () => ({ path: false }),
  token(stream, state) {
    if (stream.sol()) {
      state.path = false;
      if (stream.match(/^\s*(GET|POST|PUT|DELETE|HEAD|PATCH)(?=\s|$)/i)) {
        state.path = true;
        return "keyword";
      }
      if (stream.match(/^\s*(#|\/\/).*/)) return "comment";
    }
    if (stream.eatSpace()) return null;
    if (state.path) {
      stream.skipToEnd();
      return "link";
    }
    if (stream.match('"""')) {
      while (!stream.eol() && !stream.match('"""')) stream.next();
      return "string";
    }
    const char = stream.peek();
    if (char === '"') {
      stream.next();
      readString(stream);
      return stream.match(/^\s*:/, false) ? "propertyName" : "string";
    }
    if (stream.match(/^-?\d+(\.\d+)?([eE][+-]?\d+)?/)) return "number";
    if (stream.match(/^(true|false)\b/)) return "bool";
    if (stream.match(/^null\b/)) return "null";
    if (stream.match(/^[{}[\]]/)) return "bracket";
    stream.next();
    return "punctuation";
  },
  languageData: { closeBrackets: { brackets: ["[", "{", '"'] }, commentTokens: { line: "#" } },
});

/** Index names and fields, loaded once per connection (fields once per index). */
class NameCache {
  private indices: string[] = [];
  private indicesLoad: Promise<void> | null = null;
  private readonly fields = new Map<string, string[]>();
  private readonly fieldLoads = new Map<string, Promise<void>>();

  constructor(private readonly connection: ConnectionApi | undefined) {}

  loadIndices(): Promise<void> {
    const connection = this.connection;
    if (!connection) return Promise.resolve();
    this.indicesLoad ??= connection
      .invokeCapability<{ names: string[] }>("es.names", {})
      .then((answer) => void (this.indices = answer.names ?? []))
      .catch(() => void (this.indicesLoad = null));
    return this.indicesLoad;
  }

  loadFields(index: string): Promise<void> {
    const connection = this.connection;
    if (!connection || this.fields.has(index)) return Promise.resolve();
    let pending = this.fieldLoads.get(index);
    if (!pending) {
      pending = connection
        .invokeCapability<{ fields: { name: string }[] }>("es.fields", { index })
        .then(
          (answer) =>
            void this.fields.set(
              index,
              (answer.fields ?? []).map((field) => field.name),
            ),
        )
        .catch(() => void this.fieldLoads.delete(index));
      this.fieldLoads.set(index, pending);
    }
    return pending;
  }

  names(): ConsoleNames {
    return { indices: this.indices, fields: (index) => this.fields.get(index) ?? null };
  }
}

function completionSource(cache: NameCache): CompletionSource {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const text = context.state.doc.toString();
    await cache.loadIndices();
    const block = blocks(text)
      .filter((item) => item.from <= context.pos)
      .pop();
    const index = block && context.pos >= block.bodyFrom ? indexOfPath(block.path) : null;
    if (index) await cache.loadFields(index);
    const answer = consoleCompletions(text, context.pos, cache.names(), context.explicit);
    if (!answer || answer.options.length === 0) return null;
    return { from: answer.from, to: answer.to, options: answer.options, validFor: /^[\w.@/-]*$/ };
  };
}

export async function consoleLanguage(): Promise<LanguageSupport> {
  return {
    id: "es-console",
    async editorExtensions(ctx?: LanguageContext): Promise<Extension[]> {
      // One source per editor: the autocompletion keeps a result only while the same source is still active.
      const source = completionSource(new NameCache(ctx?.connection));
      return [consoleMode, EditorState.languageData.of(() => [{ autocomplete: source }])];
    },
    statementAt: consoleStatementAt,
    format: async (text) => formatConsole(text),
  };
}
