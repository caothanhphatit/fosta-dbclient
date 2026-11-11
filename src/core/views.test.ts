import { afterEach, describe, expect, it } from "vitest";
import type { EngineDescriptor } from "./contract";
import { clearViews, defaultRole, findView, registerViews, resolveView, rolesFor, setEngineLoader, type ViewFactory, type ViewRegistration } from "./views";
import descriptorFixture from "./__fixtures__/contract/engine-descriptor.json";

const base = descriptorFixture as EngineDescriptor;
const mariadb: EngineDescriptor = { ...base, id: "mariadb", family: "sql", capabilities: ["tree", "schemaGraph"] };
const postgres: EngineDescriptor = { ...base, id: "postgres", family: "sql", capabilities: ["tree"] };
const mongodb: EngineDescriptor = { ...base, id: "mongodb", family: "document", capabilities: ["tree"] };

const factory: ViewFactory = () => ({ element: {} as HTMLElement });
const load = () => Promise.resolve(factory);

function reg(name: string, fields: Omit<ViewRegistration, "load" | "title">): ViewRegistration {
  return { ...fields, title: () => name, load };
}

const nameOf = (registration: ViewRegistration | null) => registration?.title?.({} as never) ?? null;

afterEach(() => clearViews());

describe("view registry", () => {
  it("prefers engine+kind over family+kind over *+kind over engine+*", () => {
    registerViews([
      reg("engine-any", { engine: "mariadb", kinds: "*", role: "data" }),
      reg("any-kind", { engine: "*", kinds: ["table"], role: "data" }),
      reg("family-kind", { engine: "*", family: "sql", kinds: ["table"], role: "data" }),
      reg("engine-kind", { engine: "mariadb", kinds: ["table"], role: "data" }),
    ]);
    expect(nameOf(findView(mariadb, "table", "data"))).toBe("engine-kind");
    expect(nameOf(findView(postgres, "table", "data"))).toBe("family-kind");
    expect(nameOf(findView(mongodb, "table", "data"))).toBe("any-kind");
    expect(nameOf(findView(mariadb, "view", "data"))).toBe("engine-any");
    expect(findView(postgres, "view", "data")).toBeNull();
  });

  it("matches the role and the family", () => {
    registerViews([reg("sql-design", { engine: "*", family: "sql", kinds: ["table"], role: "design" })]);
    expect(nameOf(findView(postgres, "table", "design"))).toBe("sql-design");
    expect(findView(postgres, "table", "data")).toBeNull();
    expect(findView(mongodb, "table", "design")).toBeNull();
  });

  it("lets a later registration of the same precedence win", () => {
    registerViews([reg("shared", { engine: "*", kinds: ["collection"], role: "data" })]);
    registerViews([reg("override", { engine: "*", kinds: ["collection"], role: "data" })]);
    expect(nameOf(findView(mongodb, "collection", "data"))).toBe("override");
  });

  it("gates on a capability", () => {
    registerViews([reg("diagram", { engine: "*", kinds: "*", role: "diagram", requires: "schemaGraph" })]);
    expect(nameOf(findView(mariadb, "database", "diagram"))).toBe("diagram");
    expect(findView(postgres, "database", "diagram")).toBeNull();
  });

  it("lists the roles of a kind", () => {
    registerViews([
      reg("data", { engine: "*", family: "sql", kinds: ["table"], role: "data" }),
      reg("design", { engine: "*", family: "sql", kinds: ["table"], role: "design" }),
      reg("console", { engine: "*", kinds: "*", role: "console" }),
    ]);
    expect(rolesFor(postgres, "table")).toEqual(["data", "design", "console"]);
    expect(rolesFor(mongodb, "collection")).toEqual(["console"]);
  });

  it("opens a workspace by default where one is registered", () => {
    registerViews([
      reg("data", { engine: "*", kinds: ["collection"], role: "data" }),
      reg("workspace", { engine: "mongodb", kinds: ["collection"], role: "workspace" }),
    ]);
    expect(defaultRole(mongodb, "collection", "data")).toBe("workspace");
    expect(defaultRole(mongodb, "view", "data")).toBe("data");
    expect(defaultRole(mongodb, "collection", "design")).toBe("design");
    expect(defaultRole(postgres, "collection", "data")).toBe("data");
    expect(rolesFor(mongodb, "collection")).toEqual(["data", "workspace"]);
  });

  it("loads the engine module before resolving", async () => {
    const loaded: string[] = [];
    setEngineLoader(async (id) => {
      loaded.push(id);
      registerViews([reg("from-module", { engine: id, kinds: ["key"], role: "value" })]);
    });
    expect(nameOf(await resolveView({ ...base, id: "redis", family: "keyValue" }, "key", "value"))).toBe("from-module");
    expect(loaded).toEqual(["redis"]);
  });
});
