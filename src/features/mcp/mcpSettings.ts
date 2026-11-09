/** Settings → MCP server: on/off, port, token (desktop), and copy-paste setup for Claude Code, Cursor and Claude Desktop. */
import { api, onMcpActivity } from "../../core/api";
import { capabilities } from "../../core/capabilities";
import { checkbox, confirmDialog, field, openModal, textInput, toast } from "../../core/dialogs";
import { copyText, h, icon } from "../../core/dom";
import { errorText } from "../../core/errors";
import type { AppConfig, McpStatus } from "../../core/types";
import { claudeCodeCommand, claudeDesktopConfig, cursorConfig, statusText, webEndpoint } from "./mcpLogic";

const item = h("button", { class: "status-seg status-mcp", type: "button", hidden: true, title: "MCP server" });
let status: McpStatus | null = null;
let applyConfig: (config: AppConfig) => void = () => undefined;

/** The status bar item: shown while the server is on, with the number of tool calls; opens the settings. */
export function mcpStatusItem(): HTMLElement {
  return item;
}

function render(): void {
  const text = status ? statusText(status) : null;
  item.hidden = text === null;
  item.replaceChildren(icon("plug"), h("span", {}, text ?? ""));
  item.classList.toggle("warn", Boolean(status?.enabled && !status.running));
  const clients = status?.clients.length ? `Connected: ${status.clients.join(", ")}` : "No client connected";
  item.title = status?.error ? `MCP server: ${status.error}` : `MCP server on. ${clients}`;
}

export async function refreshMcpStatus(): Promise<void> {
  try {
    status = await api.mcpStatus();
  } catch {
    status = null;
  }
  render();
}

/** Wires the status item; `sink` receives the config after a change. */
export async function initMcpStatus(sink: (config: AppConfig) => void): Promise<void> {
  applyConfig = sink;
  item.addEventListener("click", () => void openMcpSettings());
  await onMcpActivity((activity) => {
    if (!status) return;
    status = { ...status, calls: activity.calls, clients: activity.clients };
    render();
  });
  await refreshMcpStatus();
}

function snippet(title: string, text: string): HTMLElement {
  const copy = h("button", { class: "btn small", type: "button", title: `Copy the ${title} setup` }, icon("copy"), "Copy");
  copy.addEventListener("click", () => void copyText(text).then(() => toast("Copied", "success")));
  return h("div", { class: "mcp-snippet" }, h("div", { class: "mcp-snippet-head" }, h("strong", {}, title), copy), h("pre", { class: "mono" }, text));
}

export async function openMcpSettings(): Promise<void> {
  await refreshMcpStatus();
  const current = status;
  if (!current) {
    toast("The MCP server is not available in this build", "error");
    return;
  }
  const web = capabilities().edition === "web";
  const enabled = checkbox("Run the MCP server", current.enabled);
  const port = textInput(String(current.port), { type: "number", min: "1024", max: "65535", class: "input mono" });
  let token = "";
  if (!web && current.enabled) token = await api.mcpToken().catch(() => "");
  const tokenInput = textInput(token, { class: "input mono", readonly: true, type: "password", "aria-label": "MCP token" });
  const snippets = h("div", { class: "mcp-snippets" });
  const url = web ? webEndpoint(window.location.origin, current.url) : current.url;
  const renderSnippets = () => {
    const shown = token || (web ? "<access token>" : "<token>");
    const bridge = web ? ["docker", "exec", "-i", "<container>", "/app/fosta-studio-server", "mcp"] : current.bridgeCommand;
    snippets.replaceChildren(
      snippet("Claude Code", claudeCodeCommand(url, shown)),
      snippet("Cursor (mcp.json)", cursorConfig(url, shown)),
      snippet("Claude Desktop (stdio)", claudeDesktopConfig(bridge)),
    );
  };
  renderSnippets();
  const show = h("button", { class: "btn small", type: "button" }, "Show");
  show.addEventListener("click", () => {
    tokenInput.type = tokenInput.type === "password" ? "text" : "password";
    show.textContent = tokenInput.type === "password" ? "Show" : "Hide";
  });
  const copy = h("button", { class: "btn small", type: "button" }, icon("copy"), "Copy");
  copy.addEventListener("click", () => void copyText(tokenInput.value).then(() => toast("Token copied", "success")));
  const rotate = h("button", { class: "btn small", type: "button" }, icon("refresh"), "Rotate…");
  rotate.addEventListener("click", () => {
    void (async () => {
      if (!(await confirmDialog("Make a new token? Clients set up with the current one stop working until you update them.", "Rotate"))) return;
      try {
        token = await api.rotateMcpToken();
        tokenInput.value = token;
        renderSnippets();
        toast("New MCP token", "success");
      } catch (error) {
        toast(errorText(error), "error");
      }
    })();
  });
  const body = h(
    "div",
    { class: "form-grid" },
    h(
      "p",
      { class: "form-intro" },
      "Lets AI clients use your saved connections without seeing passwords. Clients only see workspaces where Edit workspace → MCP clients allows it.",
    ),
    enabled.element,
    current.error ? h("p", { class: "mcp-error" }, current.error) : null,
    web
      ? field(
          "Endpoint",
          textInput(url, { class: "input mono", readonly: true }),
          "Clients authenticate with the server's access token: fosta-studio-server token.",
        )
      : field("Port", port, `Listens on 127.0.0.1 only: ${current.url}`),
    web
      ? null
      : field(
          "Token",
          h("div", { class: "mcp-token-row" }, tokenInput, show, copy, rotate),
          current.enabled ? "Kept in Windows Credential Manager. Treat it like a password." : "Created when the server is turned on.",
        ),
    snippets,
  );
  openModal(
    "MCP server",
    body,
    [
      { label: "Cancel", run: () => undefined },
      {
        label: "Save",
        primary: true,
        run: async () => {
          const portNumber = Number(port.value);
          if (!web && (!Number.isInteger(portNumber) || portNumber < 1024 || portNumber > 65535)) throw new Error("Pick a port from 1024 to 65535");
          applyConfig(await api.setMcpSettings(enabled.input.checked, web ? null : portNumber));
          await refreshMcpStatus();
          if (enabled.input.checked && !current.enabled && !web) {
            // Reopen with the token now in the snippets.
            window.setTimeout(() => void openMcpSettings(), 0);
          }
        },
      },
    ],
    { width: 640 },
  );
}
