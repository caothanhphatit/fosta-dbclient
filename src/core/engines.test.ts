import { afterEach, describe, expect, it } from "vitest";
import { DESCRIPTORS, ORDER } from "../dev/mock/descriptors";
import { engineInfo, isKnownFence, languageForFence, setEngines } from "./engines";

describe("assistant code fences", () => {
  afterEach(() => setEngines([]));

  it("map a block's tag to a language of the engine", () => {
    setEngines(ORDER.map((id) => DESCRIPTORS[id]));
    expect(languageForFence(engineInfo("postgres"), "sql")?.id).toBe("sql");
    expect(languageForFence(engineInfo("postgres"), "PostgreSQL")?.id).toBe("sql");
    expect(languageForFence(engineInfo("mongodb"), "json")?.id).toBe("mongo-command");
    expect(languageForFence(engineInfo("mongodb"), "sql")).toBeNull();
    expect(languageForFence(engineInfo("elasticsearch"), "http")?.id).toBe("es-console");
    expect(languageForFence(engineInfo("elasticsearch"), "sql")?.id).toBe("es-sql");
    // An untagged block runs in the default language.
    expect(languageForFence(engineInfo("redis"), "")?.id).toBe("redis");
    expect(languageForFence(engineInfo("nope"), "sql")).toBeNull();
  });

  it("know which tags any engine runs", () => {
    setEngines(ORDER.map((id) => DESCRIPTORS[id]));
    for (const tag of ["sql", "tsql", "json", "redis", "http", ""]) expect(isKnownFence(tag), tag).toBe(true);
    expect(isKnownFence("python")).toBe(false);
  });

  it("every engine writes a fence its own languages accept", () => {
    for (const id of ORDER) {
      const descriptor = DESCRIPTORS[id];
      for (const language of descriptor.languages) {
        expect(language.fences.length, `${id}/${language.id}`).toBeGreaterThan(0);
        expect(languageForFence(descriptor, language.fences[0])?.id, `${id}/${language.id}`).toBeDefined();
      }
    }
  });
});
