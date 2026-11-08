/** The settings, workspace and theme menus of the shell, and the actions they run. */
import { showError } from "../core/errorPanel";
import { api } from "../core/api";
import { capabilities } from "../core/capabilities";
import { confirmDialog, toast } from "../core/dialogs";
import { signOut } from "../core/transport";
import type { MenuItem } from "../core/contextMenu";
import type { IconName } from "../core/dom";
import { describeTheme, setTheme, themePreference, type ThemePreference } from "../core/theme";
import type { AppConfig, Workspace } from "../core/types";
import { copyDiagnostics, openLogFolder, setDebugLogging } from "./diagnostics";

const THEME_OPTIONS: { value: ThemePreference; label: string }[] = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "Follow system" },
];

/** What the menus need from the shell. */
export interface MenuHost {
  config(): AppConfig | null;
  applyConfig(config: AppConfig): void;
  setStatus(message: string): void;
  configureAssistant(): Promise<void>;
  configureEmbedding(): Promise<void>;
  editWorkspace(workspace: Workspace): Promise<void>;
  createWorkspace(): Promise<void>;
  deleteWorkspace(workspace: Workspace): Promise<void>;
}

export interface ShellMenus {
  workspaceMenu(current: Workspace): MenuItem[];
  settingsMenu(): MenuItem[];
  themeMenu(): MenuItem[];
}

export function shellMenus(host: MenuHost): ShellMenus {
  const { applyConfig, setStatus } = host;
  function workspaceMenu(current: Workspace): MenuItem[] {
    const count = host.config()?.workspaces.length ?? 1;
    return [
      { label: "Edit workspace…", icon: "edit", run: () => void host.editWorkspace(current) },
      { label: "New workspace…", icon: "plus", run: () => void host.createWorkspace() },
      "separator",
      { label: "Delete workspace", icon: "trash", danger: true, disabled: count <= 1, run: () => void host.deleteWorkspace(current) },
    ];
  }

  function settingsMenu(): MenuItem[] {
    return [
      { label: "LLM providers…", icon: "sparkle", run: () => void host.configureAssistant() },
      { label: "Embedding API…", icon: "vector", run: () => void host.configureEmbedding() },
      "separator",
      {
        label: "Record query history",
        icon: host.config()?.queryHistory !== false ? "check" : undefined,
        run: () => void setQueryHistory(host.config()?.queryHistory === false),
      },
      { label: "Clear query history…", icon: "trash", run: () => void clearQueryHistory() },
      "separator",
      {
        label: "Debug logging",
        icon: host.config()?.debugLogging ? "check" : undefined,
        run: () => void setDebugLogging(!host.config()?.debugLogging, applyConfig),
      },
      { label: "Copy diagnostics", icon: "copy", run: () => void copyDiagnostics() },
      { label: "Open log folder", icon: "logs", run: () => void openLogFolder() },
      ...(capabilities().edition === "web" ? (["separator", { label: "Sign out…", icon: "key", run: () => void confirmSignOut() }] as MenuItem[]) : []),
    ];
  }

  /** Web edition only: there is one access token per server, so signing out ends every browser's session. */
  async function confirmSignOut(): Promise<void> {
    const message = "Sign out of Fosta Studio? This signs out every browser using this server; sign in again with the access token.";
    if (!(await confirmDialog(message, "Sign out"))) return;
    try {
      await signOut();
    } catch (error) {
      showError(error, "Sign out");
    }
  }

  /** History stays on this machine (fosta-studio-history.jsonl next to the app); turning it off keeps old entries. */
  async function setQueryHistory(enabled: boolean): Promise<void> {
    try {
      applyConfig(await api.setQueryHistory(enabled));
      setStatus(enabled ? "Query history is on" : "Query history is off; new runs are not recorded");
    } catch (error) {
      showError(error, "Change query history");
    }
  }

  async function clearQueryHistory(): Promise<void> {
    if (!(await confirmDialog("Delete the whole query history of every connection on this machine?", "Delete"))) return;
    try {
      await api.historyClear();
      toast("Query history cleared", "success");
    } catch (error) {
      showError(error, "Clear the query history");
    }
  }

  /** Right-click menu of the theme button: the one place to pick "follow system". */
  function themeMenu(): MenuItem[] {
    const current = themePreference();
    return THEME_OPTIONS.map((option) => ({
      label: option.label,
      icon: option.value === current ? ("check" as IconName) : undefined,
      run: () => {
        setTheme(option.value);
        setStatus(describeTheme());
      },
    }));
  }

  return { workspaceMenu, settingsMenu, themeMenu };
}
