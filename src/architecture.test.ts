/**
 * Dependency rules between frontend layers, checked on the source text (the Rust side is `src-tauri/src/architecture_tests.rs`).
 *
 * Shared UI (`core`, `features`, `app`, `main.ts`) must not import an engine package (`src/engines/<id>/`) and must not
 * compare engine ids as string literals: it asks the descriptors (`core/engines.ts`) and the view registry
 * (`core/views.ts`) instead. Engine packages reach the app only through the registry (`src/engines/index.ts`), and
 * `main.ts` is the one place that loads it. `src/dev` (the browser mock) is development tooling and is not checked.
 */
import { describe, expect, it } from "vitest";

/** Every source file under src/ by path relative to it ("core/api.ts"), with its text. */
const FILES: Record<string, string> = Object.fromEntries(
  Object.entries(import.meta.glob<string>("./**/*.ts", { query: "?raw", import: "default", eager: true })).map(([path, text]) => [path.slice(2), text]),
);

/** Known debt: shared code that still compares engine ids. Entries may only be removed, never added. */
const ENGINE_ID_COMPARISONS: Record<string, number> = {
  "features/assistant/assistantPanel.ts": 7,
};

/** Engine packages that build on another one (OpenSearch is an Elasticsearch fork with the same wire API). */
const ENGINE_REUSE = new Set(["opensearch -> elasticsearch"]);

const isTest = (file: string) => file.endsWith(".test.ts");
const inSharedUi = (file: string) => file === "main.ts" || ["core/", "features/", "app/"].some((folder) => file.startsWith(folder));
const shared = Object.keys(FILES).filter((file) => inSharedUi(file) && !isTest(file));

/** Resolves `specifier` against the folder of `file` to a path relative to src/ ("engines/mysql/index"). */
function resolveImport(file: string, specifier: string): string {
  const parts = file.split("/").slice(0, -1);
  for (const part of specifier.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== ".") parts.push(part);
  }
  return parts.join("/");
}

/** Engine ids the build knows (`KnownEngineId` in `core/types.ts`). */
function engineIds(): string[] {
  const declaration = /export type KnownEngineId =([^;]*);/.exec(FILES["core/types.ts"])?.[1] ?? "";
  return [...declaration.matchAll(/"([a-z]+)"/g)].map((match) => match[1]);
}

/** The relative module specifiers of `import … from "x"`, `import "x"` and `import("x")`. */
export function importSpecifiers(text: string): string[] {
  const found: string[] = [];
  for (const match of text.matchAll(
    /(?:^|\n)\s*(?:import|export)\s[^"'`;]*?from\s*["']([^"']+)["']|(?:^|\n)\s*import\s*["']([^"']+)["']|import\(\s*["']([^"']+)["']\s*\)/g,
  )) {
    const specifier = match[1] ?? match[2] ?? match[3];
    if (specifier?.startsWith(".")) found.push(specifier);
  }
  return found;
}

/** Comparisons of an engine-id-like expression with an engine id literal, e.g. `profile.driver === "mongodb"`. */
export function engineIdComparisons(text: string, ids: string[]): string[] {
  const alternatives = ids.join("|");
  const subject = String.raw`(?:\w+\.)*(?:driver|engine|engineId|id)`;
  const literal = String.raw`["'](?:${alternatives})["']`;
  const forward = new RegExp(String.raw`\b${subject}\s*[!=]==?\s*${literal}`, "g");
  const backward = new RegExp(String.raw`${literal}\s*[!=]==?\s*${subject}\b`, "g");
  return [...(text.match(forward) ?? []), ...(text.match(backward) ?? [])];
}

describe("the helpers", () => {
  it("find engine imports and id comparisons", () => {
    const text = `import { a } from "../engines/mysql";\nimport "./x";\nconst m = await import("../../engines/index");\nimport { b } from "pkg";`;
    expect(importSpecifiers(text)).toEqual(["../engines/mysql", "./x", "../../engines/index"]);
    const ids = ["mongodb", "mysql"];
    expect(engineIdComparisons(`if (profile.driver === "mongodb") {}`, ids)).toHaveLength(1);
    expect(engineIdComparisons(`x = "mysql" !== info.id;`, ids)).toHaveLength(1);
    expect(engineIdComparisons(`if (dialect === "mysql") {}`, ids)).toHaveLength(0);
  });

  it("know the engine ids", () => {
    expect(engineIds()).toEqual(expect.arrayContaining(["mariadb", "mongodb", "mssql", "postgres"]));
  });
});

describe("shared UI", () => {
  it("is found", () => {
    expect(shared.length).toBeGreaterThan(50);
  });

  it("does not import engine packages, except main.ts loading the registry", () => {
    const violations: string[] = [];
    for (const file of shared) {
      const text = FILES[file];
      for (const specifier of importSpecifiers(text)) {
        const target = resolveImport(file, specifier);
        if (target !== "engines" && !target.startsWith("engines/")) continue;
        const loadsRegistry = file === "main.ts" && target === "engines";
        if (!loadsRegistry) violations.push(`${file} imports ${specifier}`);
      }
    }
    expect(violations).toEqual([]);
  });

  it("does not compare engine ids as string literals (known debt only shrinks)", () => {
    const ids = engineIds();
    const found: Record<string, number> = {};
    for (const file of shared) {
      const count = engineIdComparisons(FILES[file], ids).length;
      if (count > 0) found[file] = count;
    }
    for (const [file, count] of Object.entries(found)) {
      expect(count, `${file} compares engine ids ${count} time(s)`).toBeLessThanOrEqual(ENGINE_ID_COMPARISONS[file] ?? 0);
    }
    for (const [file, allowed] of Object.entries(ENGINE_ID_COMPARISONS)) {
      expect(found[file] ?? 0, `${file} improved: lower its allowance to ${found[file] ?? 0}`).toBe(allowed);
    }
  });
});

describe("engine packages", () => {
  it("do not import another engine's package", () => {
    const violations: string[] = [];
    for (const file of Object.keys(FILES).filter((item) => item.startsWith("engines/") && !isTest(item))) {
      const own = file.split("/")[1];
      for (const specifier of importSpecifiers(FILES[file])) {
        const target = resolveImport(file, specifier);
        if (!target.startsWith("engines/")) continue;
        const other = target.split("/")[1];
        const common = ["shared", "sql", "index"].includes(other);
        const registry = file === "engines/index.ts";
        if (other !== own && !common && !registry && !ENGINE_REUSE.has(`${own} -> ${other}`)) violations.push(`${file} imports ${specifier}`);
      }
    }
    expect(violations).toEqual([]);
  });
});
