/**
 * The shared vector views on a Redis search index (a tree node of database 0, where search indexes live): the index
 * stands for the table, its VECTOR fields for the columns and each document's key (`__key`) for the row key.
 */
import type { TableTarget } from "../../core/types";
import type { ViewContext } from "../../core/views";
import { VectorHealthView } from "../../features/vector/vectorHealth";
import { VectorSearchView } from "../../features/vector/vectorSearch";

function indexTarget(ctx: ViewContext): TableTarget {
  return {
    connectionId: ctx.connection.connectionId,
    driver: ctx.profile.driver,
    database: ctx.object?.database ?? "0",
    table: ctx.object?.name ?? "",
    kind: "table",
  };
}

/** The first vector field is selected; the view lists the others. */
export const vectorSearchViewFactory = (ctx: ViewContext): VectorSearchView => new VectorSearchView(indexTarget(ctx), "", ctx.navigator, null);

export const vectorHealthViewFactory = (ctx: ViewContext): VectorHealthView => new VectorHealthView(indexTarget(ctx), "");
