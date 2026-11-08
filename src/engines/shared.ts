/**
 * Views every engine gets unless its UI module registers its own: the table grid and the design view for tabular
 * objects, the query tab as the console, and the ER diagram where the engine has `schemaGraph`. The data, design and
 * diagram views take the `ViewContext` (the injected `ConnectionApi`); the query view moves onto it in step 14.
 */
import type { ConnectionProfile } from "../core/types";
import type { ViewContext, ViewFactory, ViewRegistration } from "../core/views";
// The grid, design and diagram views are in the main chunk today (app/navigation.ts imports them); `load` stays async so
// they can move to chunks of their own once navigation opens every view through the registry (step 12).
import { DesignView } from "../features/design/designView";
import { DiagramView } from "../features/diagram/diagramView";
import type { QueryHost } from "../features/query/queryView";
import { TableView } from "../features/table/tableView";

/** Object kinds the shared grid and design view handle for SQL engines. */
export const SQL_TABULAR_KINDS = ["table", "view", "materializedView", "foreignTable", "dictionary"];

function databaseOf(ctx: ViewContext): string | null {
  const fromParams = ctx.params?.database;
  if (typeof fromParams === "string") return fromParams;
  return ctx.object?.database ?? null;
}

function defaultQueryHost(profile: ConnectionProfile): QueryHost {
  return {
    connectionName: () => profile.name,
    historyEnabled: () => true,
    openQuery: () => undefined,
  };
}

const dataView = async (): Promise<ViewFactory> => {
  return (ctx) => new TableView(ctx);
};

const designView = async (): Promise<ViewFactory> => {
  return (ctx) => new DesignView(ctx);
};

export const sharedViews: ViewRegistration[] = [
  { engine: "*", family: "sql", kinds: SQL_TABULAR_KINDS, role: "data", icon: "table", load: dataView },
  { engine: "*", family: "sql", kinds: SQL_TABULAR_KINDS, role: "design", icon: "design", load: designView },
  { engine: "*", kinds: ["collection"], role: "data", icon: "table", load: dataView },
  { engine: "*", kinds: ["collection"], role: "design", icon: "design", load: designView },
  {
    engine: "*",
    kinds: "*",
    role: "console",
    icon: "query",
    title: () => "Query",
    load: async () => {
      const { QueryView } = await import("../features/query/queryView");
      return (ctx) => {
        const host = (ctx.params?.host as QueryHost | undefined) ?? defaultQueryHost(ctx.profile);
        const text = typeof ctx.params?.text === "string" ? ctx.params.text : "";
        return new QueryView(ctx.profile.id, ctx.profile.driver, databaseOf(ctx), text, host);
      };
    },
  },
  {
    engine: "*",
    kinds: "*",
    role: "diagram",
    requires: "schemaGraph",
    icon: "diagram",
    title: (ctx) => `${databaseOf(ctx) ?? ""} · diagram`,
    load: async () => {
      return (ctx) => {
        const database = databaseOf(ctx);
        if (!database) throw new Error("Pick a database for the diagram");
        return new DiagramView(ctx, database);
      };
    },
  },
];
