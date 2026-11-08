// The Redis console `LanguageSupport` (language id "redis"): command completion and the line under the cursor.
import { EditorState, type Extension } from "@codemirror/state";
import type { LanguageSupport } from "../types";
import { redisCompletionSource, redisStatementAt } from "./completion";

export interface RedisLanguageOptions {
  /** Key names to offer at key positions (for example the keys the key browser has scanned). */
  keys?: () => string[];
}

export function createRedisLanguage(options: RedisLanguageOptions = {}): LanguageSupport {
  const source = redisCompletionSource(options.keys);
  return {
    id: "redis",
    async editorExtensions(): Promise<Extension[]> {
      return [redisEditorExtension(source)];
    },
    statementAt: redisStatementAt,
  };
}

function redisEditorExtension(source: ReturnType<typeof redisCompletionSource>): Extension {
  return EditorState.languageData.of(() => [{ autocomplete: source }]);
}
