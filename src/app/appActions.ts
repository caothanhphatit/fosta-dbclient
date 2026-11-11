/**
 * The app's own actions the assistant may suggest as a chat button (`suggest_app_action`). The ids are the `action:` ids
 * of `docs/app-help.yaml`: the backend refuses any other id, and tests on both sides keep this list and the help pack
 * equal (`appActions.test.ts`, `ai::app_help` tests). An action runs only when the user clicks its button.
 */
import { copyDiagnostics, openLogFolder } from "./diagnostics";
import { checkForUpdatesNow } from "./update";
import { openMcpSettings } from "../features/mcp/mcpSettings";

export const APP_ACTION_IDS = [
  "open_dialog:edit_workspace",
  "open_dialog:new_command",
  "open_dialog:new_connection",
  "open_dialog:new_workspace",
  "open_settings:embedding",
  "open_settings:llm_providers",
  "open_settings:mcp",
  "open_view:new_query",
  "run:check_for_updates",
  "run:copy_diagnostics",
  "run:open_log_folder",
] as const;

export type AppActionId = (typeof APP_ACTION_IDS)[number];

/** The shell's own dialogs and views the actions open. */
export interface AppActionHost {
  newConnection(): void;
  newWorkspace(): void;
  /** Edits the active workspace. */
  editWorkspace(): void;
  newCommand(): void;
  newQuery(): void;
  llmProviders(): void;
  embeddingSettings(): void;
}

export type AppActionRegistry = Record<AppActionId, () => void>;

/** Each id mapped to the function its menu item or button runs. */
export function appActionRegistry(host: AppActionHost): AppActionRegistry {
  return {
    "open_dialog:edit_workspace": () => host.editWorkspace(),
    "open_dialog:new_command": () => host.newCommand(),
    "open_dialog:new_connection": () => host.newConnection(),
    "open_dialog:new_workspace": () => host.newWorkspace(),
    "open_settings:embedding": () => host.embeddingSettings(),
    "open_settings:llm_providers": () => host.llmProviders(),
    "open_settings:mcp": () => void openMcpSettings(),
    "open_view:new_query": () => host.newQuery(),
    "run:check_for_updates": () => void checkForUpdatesNow(),
    "run:copy_diagnostics": () => void copyDiagnostics(),
    "run:open_log_folder": () => void openLogFolder(),
  };
}

export function isAppActionId(id: string): id is AppActionId {
  return (APP_ACTION_IDS as readonly string[]).includes(id);
}
