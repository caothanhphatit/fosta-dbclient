import type { TabContent } from "../core/tabs";
import type { AppConfig, TaskDef } from "../core/types";
import type { AssistantContext } from "../features/assistant/assistantPanel";
import type { ConnectionTree } from "../features/connections/tree";
import type { QueryView } from "../features/query/queryView";
import { TableView } from "../features/table/tableView";
import type { LogTabContent } from "../features/tasks/tasksPanel";
import { isWorkspaceTab } from "../features/objectWorkspace/sectionWorkspace";
import { ProfileView } from "../features/assistant/profileView";

const LOG_TAIL_LINES = 80;

/** What the model is told about a workspace command: its program and the end of its log. */
export function taskLogContext(task: TaskDef, lines: string[]): string {
  return `Command "${task.name}" runs: ${task.program} ${task.args.join(" ")}\nLast log lines:\n${lines.slice(-LOG_TAIL_LINES).join("\n")}`;
}

/** Duck-typed so the CodeMirror chunk is not pulled into the main bundle. */
export function isQueryView(content: TabContent | null): content is QueryView {
  return content !== null && typeof (content as QueryView).currentText === "function";
}

function isLogTab(content: TabContent | null): content is LogTabContent {
  return content !== null && typeof (content as LogTabContent).logLines === "function";
}

/** What the assistant should know about the active tab (or the tree selection). */
export function assistantContext(active: TabContent | null, config: AppConfig | null, tree: ConnectionTree): AssistantContext {
  const withConnection = (connectionId: string, rest: Partial<AssistantContext>): AssistantContext => {
    const profile = config?.workspaces.flatMap((item) => item.connections).find((item) => item.id === connectionId);
    const connected = tree.connectedIds().has(connectionId);
    return {
      kind: "connection",
      connectionId: connected ? connectionId : undefined,
      connectionName: profile?.name,
      driver: profile?.driver,
      serverVersion: tree.serverVersion(connectionId) ?? undefined,
      ...rest,
    };
  };
  if (isQueryView(active)) {
    return withConnection(active.connectionId, {
      kind: "query",
      database: active.currentDatabase() ?? undefined,
      editorText: () => active.currentText(),
      lastError: () => active.lastError,
    });
  }
  if (isWorkspaceTab(active)) {
    // An engine workspace: the object, plus what each loaded section says about itself (bounded in `workspaceContextText`).
    const object = active.object;
    return withConnection(active.connectionId, {
      kind: "table",
      database: object?.database ?? undefined,
      table: object ? (object.schema ? `${object.schema}.${object.name}` : object.name) : undefined,
      object,
      extra: () => active.workspaceContext(),
      // Row data, apart from the context: the backend drops it where the assistant may not read data.
      samples: () => active.workspaceSamples(),
    });
  }
  if (active instanceof TableView) {
    return withConnection(active.target.connectionId, { kind: "table", database: active.target.database, table: active.target.table });
  }
  if (active instanceof ProfileView) {
    // "What the assistant knows": the chat is about that database.
    return withConnection(active.target.connectionId, { database: active.target.database || undefined });
  }
  if (isLogTab(active)) {
    const task = active.task;
    const selection = tree.activeContext();
    const base: AssistantContext = selection ? withConnection(selection.connectionId, { database: selection.database ?? undefined }) : { kind: "none" };
    return {
      ...base,
      kind: "task",
      extra: () => taskLogContext(task, active.logLines()),
    };
  }
  const selection = tree.activeContext();
  return selection ? withConnection(selection.connectionId, { database: selection.database ?? undefined }) : { kind: "none" };
}
