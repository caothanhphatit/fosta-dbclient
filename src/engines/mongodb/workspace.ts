/**
 * The MongoDB collection workspace: the tab a collection opens in by default (double-click, Enter), with its sections.
 * Documents (query bar and the matching documents) is the first; Aggregations, Schema, Indexes, Explain and
 * Validation join as they are built. The grid stays in the node menu ("Open as grid").
 */
import type { ViewContext } from "../../core/views";
import { createSectionWorkspace, type WorkspaceSection, type WorkspaceTab } from "../../features/objectWorkspace/sectionWorkspace";

export const SECTIONS: WorkspaceSection[] = [
  { id: "documents", title: "Documents", icon: "table", load: async () => (await import("./documentsView")).documentsSectionFactory },
];

export function collectionWorkspaceFactory(ctx: ViewContext): WorkspaceTab {
  return createSectionWorkspace(ctx, SECTIONS);
}
