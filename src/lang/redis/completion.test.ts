import { describe, expect, it } from "vitest";
import { cursor } from "../testSchema";
import { arityError, findCommand, isKeyArgument, optionTokens, REDIS_COMMANDS } from "./commands";
import { quoteArg, redisCompletions, redisStatementAt, redisWords } from "./completion";

const KEYS = ["user:1", "user:2", "session token"];
const complete = (text: string, explicit = true) => {
  const { doc, pos } = cursor(text);
  return redisCompletions(doc, pos, KEYS, explicit);
};
const labels = (text: string) => complete(text)?.options.map((o) => o.label) ?? [];

describe("Redis command table", () => {
  it("has unique names and sane arities", () => {
    const names = REDIS_COMMANDS.flatMap((c) => [c.name, ...(c.subcommands ?? []).map((s) => s.name)]);
    expect(new Set(names).size).toBe(names.length);
    for (const c of REDIS_COMMANDS) expect(c.arity).not.toBe(0);
  });

  it("finds commands and subcommands case-insensitively", () => {
    expect(findCommand(["get", "k"])?.name).toBe("GET");
    expect(findCommand(["config", "get", "x"])?.name).toBe("CONFIG GET");
    expect(findCommand(["nope"])).toBeUndefined();
  });

  it("extracts option tokens and key positions", () => {
    const set = findCommand(["SET"]);
    expect(optionTokens(set?.args ?? "")).toEqual(["NX", "XX", "GET", "EX", "PX", "EXAT", "PXAT", "KEEPTTL"]);
    expect(isKeyArgument(set!, 1)).toBe(true);
    expect(isKeyArgument(set!, 2)).toBe(false);
    expect(isKeyArgument(findCommand(["DEL"])!, 3)).toBe(true);
    expect(isKeyArgument(findCommand(["MSET"])!, 3)).toBe(true);
    expect(isKeyArgument(findCommand(["MSET"])!, 2)).toBe(false);
  });

  it("checks arity", () => {
    expect(arityError(["GET", "k"])).toBeNull();
    expect(arityError(["GET"])).toContain("GET takes 1 argument(s)");
    expect(arityError(["SET", "k"])).toContain("at least 2");
    expect(arityError(["CONFIG", "GET", "maxmemory"])).toBeNull();
    expect(arityError(["CONFIG", "GET"])).toContain("CONFIG GET takes at least 1");
    expect(arityError(["PING"])).toBeNull();
    expect(arityError(["FOO"])).toBe("Unknown command 'FOO'.");
  });
});

describe("Redis completion", () => {
  it("completes command names in the case typed", () => {
    expect(labels("SE|")).toContain("SET");
    expect(labels("se|")).toContain("set");
    expect(complete("SE|")?.options.find((o) => o.label === "SET")?.info).toContain("SET key value");
  });

  it("completes subcommands of container commands", () => {
    expect(labels("CONFIG |")).toEqual(["GET", "SET", "RESETSTAT", "REWRITE"]);
    expect(labels("client k|")).toContain("kill");
  });

  it("offers keys at key positions and unused options afterwards", () => {
    expect(labels("GET |")).toEqual(KEYS);
    expect(complete("GET |")?.options.find((o) => o.label === "session token")?.apply).toBe('"session token"');
    const afterValue = labels("SET user:1 v |");
    expect(afterValue).toContain("EX");
    expect(afterValue).not.toContain("user:1");
    expect(labels("SET user:1 v NX |")).not.toContain("NX");
    expect(labels("ZRANGE z 0 -1 |")).toEqual(expect.arrayContaining(["BYSCORE", "REV", "LIMIT", "WITHSCORES"]));
  });

  it("only completes on demand when nothing is being typed", () => {
    expect(complete("GET |", false)).toBeNull();
    expect(complete("GET u|", false)?.options.length).toBeGreaterThan(0);
  });
});

describe("Redis helpers", () => {
  it("splits words like redis-cli", () => {
    expect(redisWords(`SET "a b" 'c d' e"f g" "x\\"y"`).map((w) => w.text)).toEqual(["SET", "a b", "c d", "ef g", 'x"y']);
  });

  it("quotes arguments when needed", () => {
    expect(quoteArg("plain")).toBe("plain");
    expect(quoteArg('a "b"')).toBe('"a \\"b\\""');
    expect(quoteArg("")).toBe('""');
  });

  it("runs the current line", () => {
    const text = "GET a\n  SET b 1  \r\n\nPING";
    const at = (o: number) => {
      const r = redisStatementAt(text, o);
      return text.slice(r.from, r.to);
    };
    expect(at(2)).toBe("GET a");
    expect(at(8)).toBe("SET b 1");
    expect(at(text.length)).toBe("PING");
    expect(at(text.indexOf("\n\n") + 1)).toBe("");
  });
});
