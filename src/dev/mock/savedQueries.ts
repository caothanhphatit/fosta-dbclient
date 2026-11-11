/**
 * Saved queries in the dev mock, like `commands/saved_queries.rs`: kept on the workspace of their connection (else the
 * workspace named, else the active one), the same id replaces, a name is required.
 */
import type { SavedQuery, Workspace } from "../../core/types";
import { mockError } from "./errors";
import { state } from "./state";

const MAX_NAME_CHARS = 200;

function scope(connectionId: string | null, workspaceId: string | null): Workspace {
  const workspaces = state.config.workspaces;
  const holder = connectionId ? workspaces.find((workspace) => workspace.connections.some((profile) => profile.id === connectionId)) : undefined;
  if (holder) return holder;
  const id = workspaceId ?? state.config.activeWorkspace;
  const workspace = workspaces.find((item) => item.id === id);
  if (!workspace) throw mockError("ERROR", `Workspace '${id}' no longer exists`);
  return workspace;
}

function validName(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) throw mockError("ERROR", "A saved query needs a name");
  if (trimmed.length > MAX_NAME_CHARS) throw mockError("ERROR", `A saved query name is at most ${MAX_NAME_CHARS} characters`);
  return trimmed;
}

function update(change: (workspace: Workspace) => Workspace): void {
  state.config = { ...state.config, workspaces: state.config.workspaces.map(change) };
}

function listOf(workspaceId: string): SavedQuery[] {
  return state.config.workspaces.find((workspace) => workspace.id === workspaceId)?.savedQueries ?? [];
}

export function listSavedQueries(connectionId: string | null, workspaceId: string | null): SavedQuery[] {
  return scope(connectionId, workspaceId).savedQueries ?? [];
}

export function saveSavedQuery(query: SavedQuery, workspaceId: string | null): SavedQuery[] {
  if (!query.id?.trim()) throw mockError("ERROR", "A saved query needs an id");
  const saved: SavedQuery = { ...query, id: query.id.trim(), name: validName(query.name), updatedAt: Date.now() };
  const target = scope(saved.connectionId ?? null, workspaceId).id;
  update((workspace) => {
    const others = (workspace.savedQueries ?? []).filter((item) => item.id !== saved.id);
    if (workspace.id !== target) return { ...workspace, savedQueries: others };
    const list = [...(workspace.savedQueries ?? [])];
    const index = list.findIndex((item) => item.id === saved.id);
    if (index >= 0) list[index] = saved;
    else list.push(saved);
    return { ...workspace, savedQueries: list };
  });
  return listOf(target);
}

export function renameSavedQuery(id: string, name: string): SavedQuery[] {
  const trimmed = validName(name);
  const holder = state.config.workspaces.find((workspace) => workspace.savedQueries?.some((item) => item.id === id));
  if (!holder) throw mockError("ERROR", "The saved query no longer exists");
  update((workspace) =>
    workspace.id !== holder.id
      ? workspace
      : { ...workspace, savedQueries: workspace.savedQueries?.map((item) => (item.id === id ? { ...item, name: trimmed, updatedAt: Date.now() } : item)) },
  );
  return listOf(holder.id);
}

export function deleteSavedQuery(id: string): SavedQuery[] {
  const holder = state.config.workspaces.find((workspace) => workspace.savedQueries?.some((item) => item.id === id));
  if (!holder) throw mockError("ERROR", "The saved query no longer exists");
  update((workspace) => (workspace.id !== holder.id ? workspace : { ...workspace, savedQueries: workspace.savedQueries?.filter((item) => item.id !== id) }));
  return listOf(holder.id);
}
