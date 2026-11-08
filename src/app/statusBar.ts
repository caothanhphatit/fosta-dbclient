import { connectInfo, describeServer, engineInfo } from "../core/engines";
import { h } from "../core/dom";
import type { TabContent } from "../core/tabs";
import type { AssistantContext } from "../features/assistant/assistantPanel";
import { isQueryView } from "./assistantContext";

/** The status bar's context segments: connection, engine and server version, database, transaction, last run. */
export function renderStatusContext(target: HTMLElement, context: AssistantContext, active: TabContent | null): void {
  const segments: HTMLElement[] = [];
  const add = (text: string, title: string, className = "") => segments.push(h("span", { class: `status-seg ${className}`.trim(), title }, text));
  if (context.connectionId && context.connectionName) {
    add(context.connectionName, "Connection");
    const info = connectInfo(context.connectionId);
    const server = info ? describeServer(info) : [engineInfo(context.driver).label, context.serverVersion].filter(Boolean).join(" ");
    if (server) add(server, "Engine and server version");
    if (context.database) add(context.database, "Database");
  }
  if (isQueryView(active)) {
    const state = active.statusInfo();
    if (state.transaction) add(state.transaction, "Transaction state", state.transactionOpen ? "warn" : "");
    if (state.lastRun) add(state.lastRun, "Last run");
  }
  target.replaceChildren(...segments);
}
