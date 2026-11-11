/**
 * The MongoDB collection workspace: the tab a collection opens in by default (double-click, Enter), with its sections.
 * Documents (query bar and the matching documents), Aggregations (the pipeline builder with a per-stage preview) and
 * Schema (the shape of a sample: paths, types, presence, values); Indexes, Explain and Validation join as they are
 * built. The grid stays in the node menu ("Open as grid").
 */
import type { ViewContext } from "../../core/views";
import { createSectionWorkspace, type WorkspaceSection, type WorkspaceTab } from "../../features/objectWorkspace/sectionWorkspace";

export const SECTIONS: WorkspaceSection[] = [
  { id: "documents", title: "Documents", icon: "table", load: async () => (await import("./documentsView")).documentsSectionFactory },
  { id: "aggregations", title: "Aggregations", icon: "layers", load: async () => (await import("./aggregationsView")).aggregationsSectionFactory },
  { id: "schema", title: "Schema", icon: "diagram", load: async () => (await import("./schemaView")).schemaSectionFactory },
];

export function collectionWorkspaceFactory(ctx: ViewContext): WorkspaceTab {
  return createSectionWorkspace(ctx, SECTIONS);
}
