/**
 * The assistant panel: a chat docked on the right of the workspace. The model
 * writes queries; it never runs them. Every code block it produces carries
 * "Insert" and "New tab" buttons so the user runs the SQL from the editor.
 */
import { api, errorText } from "../../core/api";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton, spacer, type IconName } from "../../core/dom";
import { driverInfo, DRIVERS } from "../../core/drivers";
import { ModelSwitcher } from "./modelSwitcher";
import type { AiActive, AiConnection, AppConfig, ChatContext, ChatEvent, ChatMessage, DriverId, UiAction } from "../../core/types";

/** What the UI knows about where the user is. Getters are read at send time. */
export interface AssistantContext {
  kind: "none" | "connection" | "query" | "table" | "task";
  connectionId?: string;
  connectionName?: string;
  driver?: DriverId;
  serverVersion?: string;
  database?: string;
  table?: string;
  /** Text of the active query editor (selection or all). */
  editorText?: () => string;
  lastError?: () => string | null;
  /** Free-form extra context, e.g. "Startup task kubectl … Last log lines: …". */
  extra?: () => string;
}

export interface AssistantHost {
  context(): AssistantContext;
  /** Puts `text` into the active query editor, or opens a query tab for it. */
  insertSql(text: string): void;
  openQuery(text: string): void;
  /** Opens what the model asked for (diagram, table, query tab) on the current connection. */
  perform(action: UiAction): void;
  /** Opens the LLM provider dialog for the active connection (or a new one). */
  configure(): void;
  /** Opens the LLM provider dialog for `connection`, or an empty one to add a provider. */
  editProvider(connection: AiConnection | null): void;
  configChanged(config: AppConfig): void;
  close(): void;
}

interface QuickAction {
  label: string;
  icon: IconName;
  /** `send` goes straight to the model; `prefill` puts a template in the composer. */
  mode: "send" | "prefill";
  prompt: () => string;
}

interface Conversation {
  messages: ChatMessage[];
  log: HTMLElement;
}

const MAX_HISTORY = 30;
const INSERTABLE = new Set(["sql", "json", "mongodb", "mysql", "tsql", "mariadb", ""]);
const CODE_FENCE = /^```([\w+-]*)\s*$/;

export class AssistantPanel {
  readonly element: HTMLElement;
  private readonly host: AssistantHost;
  private readonly modelBadge = h("span", { class: "assistant-model" });
  private readonly contextRow = h("div", { class: "assistant-context" });
  private readonly logHost = h("div", { class: "chat-log-host" });
  private readonly actions = h("div", { class: "chat-actions" });
  private readonly composer = h("textarea", { class: "chat-input", rows: "1", placeholder: "Ask about your data, or describe the query you need…", spellcheck: "false" });
  private readonly sendButton: HTMLButtonElement;
  private readonly stopButton: HTMLButtonElement;
  private readonly setup: HTMLElement;
  private readonly chat: HTMLElement;
  private readonly conversations = new Map<string, Conversation>();
  private conversation: Conversation = { messages: [], log: h("div", { class: "chat-log" }) };
  private readonly switcher: ModelSwitcher;
  private configured = false;
  private activeRequest: string | null = null;

  constructor(host: AssistantHost) {
    this.host = host;
    this.sendButton = iconButton("send", "Send (Enter)", () => void this.send());
    this.sendButton.classList.add("chat-send");
    this.stopButton = iconButton("stop", "Stop", () => this.stop());
    this.stopButton.classList.add("chat-send", "chat-stop");
    this.stopButton.hidden = true;
    this.composer.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        void this.send();
      }
    });
    this.composer.addEventListener("input", () => this.growComposer());
    this.switcher = new ModelSwitcher({
      edit: (connection) => host.editProvider(connection),
      configChanged: (config) => host.configChanged(config),
      focusComposer: () => this.composer.focus(),
    });

    const header = h(
      "div",
      { class: "assistant-header" },
      icon("sparkle", "assistant-icon"),
      h("span", { class: "assistant-title" }, "Assistant"),
      this.modelBadge,
      spacer(),
      iconButton("plus", "New conversation", () => this.reset()),
      iconButton("settings", "LLM providers", () => host.configure()),
      iconButton("x", "Close panel", () => host.close()),
    );
    const configureButton = h("button", { class: "btn primary", type: "button" }, "Set up…");
    configureButton.addEventListener("click", () => host.configure());
    this.setup = h(
      "div",
      { class: "assistant-setup" },
      icon("sparkle", "assistant-setup-icon"),
      h("h3", {}, "Bring your own model"),
      h("p", {}, "Connect Anthropic, OpenAI, Ollama or any OpenAI-compatible endpoint with your key. The key stays in Windows Credential Manager; requests go straight from this computer to the provider."),
      h("p", {}, "The assistant sees table and column names, your editor text and errors. It writes queries; you run them."),
      configureButton,
    );
    this.chat = h(
      "div",
      { class: "assistant-chat" },
      this.logHost,
      this.actions,
      h("div", { class: "chat-composer" }, this.composer, this.sendButton, this.stopButton),
      this.switcher.element,
    );
    this.element = h("aside", { class: "assistant" }, header, this.contextRow, this.setup, this.chat);
    this.logHost.append(this.conversation.log);
    this.setConfigured([], null);
  }

  setConfigured(connections: AiConnection[], active: AiActive | null): void {
    this.configured = connections.length > 0;
    const connection = connections.find((item) => item.id === active?.connectionId);
    this.modelBadge.textContent = active?.model ?? "";
    this.modelBadge.title = connection && active ? `${connection.name} · ${active.model}` : "";
    this.switcher.set(connections, active);
    if (!this.configured) this.switcher.close();
    this.setup.hidden = this.configured;
    this.chat.hidden = !this.configured;
  }

  /** One conversation per workspace; switching keeps the others. */
  setScope(scope: string): void {
    let conversation = this.conversations.get(scope);
    if (!conversation) {
      conversation = { messages: [], log: h("div", { class: "chat-log" }) };
      this.conversations.set(scope, conversation);
    }
    this.conversation = conversation;
    this.logHost.replaceChildren(conversation.log);
    this.refreshContext();
  }

  /** Re-reads the host context: chip under the header and quick actions. */
  refreshContext(): void {
    const context = this.host.context();
    this.contextRow.replaceChildren(...contextChip(context));
    this.actions.replaceChildren(...this.quickActions(context).map((action) => this.actionChip(action)));
    this.actions.hidden = this.actions.childElementCount === 0;
  }

  focus(): void {
    this.composer.focus();
  }

  /**
   * Sends a prepared question, e.g. from an "Ask assistant" button elsewhere.
   * `extra` is added to the context of this message only (a task log, say),
   * so the answer does not depend on which tab happens to be active.
   */
  ask(prompt: string, extra?: string): void {
    this.composer.value = prompt;
    void this.send(extra);
  }

  private reset(): void {
    this.stop();
    this.conversation.messages = [];
    this.conversation.log.replaceChildren();
  }

  private async send(extra?: string): Promise<void> {
    const text = this.composer.value.trim();
    if (!text || this.activeRequest || !this.configured) return;
    this.composer.value = "";
    this.growComposer();
    const current = this.host.context();
    const context: AssistantContext = extra ? { ...current, extra: () => extra } : current;
    this.append({ role: "user", content: text });
    const bubble = this.bubble("assistant");
    const body = bubble.querySelector<HTMLElement>(".chat-body")!;
    const id = crypto.randomUUID();
    this.activeRequest = id;
    this.setBusy(true);
    let reply = "";
    const render = () => body.replaceChildren(...this.renderMarkdown(reply));
    body.append(h("span", { class: "chat-cursor" }));
    try {
      const request = { id, messages: this.conversation.messages.slice(-MAX_HISTORY), context: chatContext(context) };
      const full = await api.aiChat(request, (event: ChatEvent) => {
        if (event.type === "text") {
          reply += event.text;
          render();
        } else if (event.type === "tool") {
          bubble.before(h("div", { class: "chat-tool" }, icon("search"), event.label));
          this.scrollToEnd();
        } else if (event.type === "action") {
          this.host.perform(event.action);
        }
      });
      reply = full || reply;
      render();
      this.conversation.messages.push({ role: "assistant", content: reply });
    } catch (error) {
      const message = errorText(error);
      if (reply) this.conversation.messages.push({ role: "assistant", content: reply });
      render();
      body.append(h("div", { class: "chat-error" }, message === "Stopped" ? "Stopped." : message));
    } finally {
      this.activeRequest = null;
      this.setBusy(false);
      this.scrollToEnd();
    }
  }

  private stop(): void {
    if (this.activeRequest) void api.aiCancel(this.activeRequest);
  }

  private setBusy(busy: boolean): void {
    this.sendButton.hidden = busy;
    this.stopButton.hidden = !busy;
    this.element.classList.toggle("busy", busy);
  }

  private append(message: ChatMessage): void {
    this.conversation.messages.push(message);
    const bubble = this.bubble(message.role);
    bubble.querySelector(".chat-body")!.append(...this.renderMarkdown(message.content));
    this.scrollToEnd();
  }

  private bubble(role: ChatMessage["role"]): HTMLElement {
    const bubble = h("div", { class: `chat-message ${role}` }, h("div", { class: "chat-body" }));
    this.conversation.log.append(bubble);
    return bubble;
  }

  private scrollToEnd(): void {
    this.logHost.scrollTop = this.logHost.scrollHeight;
  }

  private growComposer(): void {
    this.composer.style.height = "auto";
    this.composer.style.height = `${Math.min(this.composer.scrollHeight, 160)}px`;
  }

  // --- Quick actions -----------------------------------------------------

  private quickActions(context: AssistantContext): QuickAction[] {
    const sql = () => fence(context.editorText?.() ?? "", context.driver === "mongodb" ? "json" : "sql");
    const table = context.table ?? "this table";
    const info = driverInfo(context.driver);
    switch (context.kind) {
      case "query": {
        const actions: QuickAction[] = [
          { label: "Explain", icon: "view", mode: "send", prompt: () => `Explain what this does, step by step:\n${sql()}` },
          { label: "Optimize", icon: "bolt", mode: "send", prompt: () => `Suggest how to make this faster (indexes, rewrites) and show the improved version:\n${sql()}` },
        ];
        if (context.lastError?.()) {
          actions.unshift({ label: "Fix error", icon: "health", mode: "send", prompt: () => `This failed with:\n${context.lastError?.()}\n\n${sql()}\nFix it and explain the cause in one sentence.` });
        }
        if (context.driver === "mongodb") {
          actions.push({ label: "Aggregation pipeline", icon: "layers", mode: "prefill", prompt: () => "Write an aggregation pipeline that " });
        } else {
          actions.push({ label: "Translate to…", icon: "globe", mode: "prefill", prompt: () => "" });
        }
        return actions;
      }
      case "table":
        return [
          { label: "Describe", icon: "design", mode: "send", prompt: () => `Describe ${table}: what each column is for, the keys, and how it relates to other tables.` },
          { label: "Test data", icon: "table", mode: "send", prompt: () => `Generate 20 rows of realistic test data for ${table} as ${info.id === "mongodb" ? "an insertMany command" : "INSERT statements"}.` },
          { label: "Query this table", icon: "query", mode: "prefill", prompt: () => `Write a query on ${table} that ` },
        ];
      case "task":
        return [
          { label: "Why does it fail?", icon: "health", mode: "send", prompt: () => "This startup task is not working. Read the log, explain what is wrong and how to fix it." },
          { label: "Explain the log", icon: "logs", mode: "send", prompt: () => "Explain what this log says, briefly." },
        ];
      case "connection":
        return [
          { label: "Write a query", icon: "query", mode: "prefill", prompt: () => "Write a query that " },
          ...(context.driver === "mongodb" ? [{ label: "Aggregation pipeline", icon: "layers" as IconName, mode: "prefill" as const, prompt: () => "Write an aggregation pipeline that " }] : []),
        ];
      default:
        return [];
    }
  }

  private actionChip(action: QuickAction): HTMLElement {
    const chip = h("button", { class: "chat-chip", type: "button" }, icon(action.icon), action.label);
    chip.addEventListener("click", (event) => {
      if (action.label === "Translate to…") {
        showContextMenu(event, this.translateMenu());
        return;
      }
      const prompt = action.prompt();
      if (action.mode === "send") this.ask(prompt);
      else {
        this.composer.value = prompt;
        this.growComposer();
        this.composer.focus();
        this.composer.setSelectionRange(prompt.length, prompt.length);
      }
    });
    return chip;
  }

  private translateMenu(): MenuItem[] {
    const context = this.host.context();
    return DRIVERS.filter((driver) => driver.id !== context.driver && driver.id !== "mariadb").map((driver) => ({
      label: driver.id === "mongodb" ? "MongoDB (command / pipeline)" : driver.label,
      icon: driver.icon,
      run: () => {
        const source = fence(context.editorText?.() ?? "", "sql");
        const target = driver.id === "mongodb" ? "a MongoDB command or aggregation pipeline" : driver.label;
        this.ask(`Translate this to ${target}, keeping the same result. Mention anything without a direct equivalent:\n${source}`);
      },
    }));
  }

  // --- Markdown-lite -----------------------------------------------------

  private renderMarkdown(text: string): HTMLElement[] {
    const nodes: HTMLElement[] = [];
    const lines = text.split("\n");
    let paragraph: string[] = [];
    let list: { ordered: boolean; items: string[] } | null = null;
    const flushParagraph = () => {
      if (paragraph.length) nodes.push(h("p", {}, ...inline(paragraph.join(" "))));
      paragraph = [];
    };
    const flushList = () => {
      if (list) nodes.push(h(list.ordered ? "ol" : "ul", {}, ...list.items.map((item) => h("li", {}, ...inline(item)))));
      list = null;
    };
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      const fenceMatch = CODE_FENCE.exec(line.trim());
      if (fenceMatch) {
        flushParagraph();
        flushList();
        const language = fenceMatch[1].toLowerCase();
        const code: string[] = [];
        index += 1;
        while (index < lines.length && !lines[index].trim().startsWith("```")) {
          code.push(lines[index]);
          index += 1;
        }
        nodes.push(this.codeBlock(code.join("\n"), language));
        continue;
      }
      const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
      const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
      if (bullet || numbered) {
        flushParagraph();
        const ordered = Boolean(numbered);
        if (!list || list.ordered !== ordered) {
          flushList();
          list = { ordered, items: [] };
        }
        list.items.push((bullet ?? numbered)![1]);
        continue;
      }
      if (!line.trim()) {
        flushParagraph();
        flushList();
        continue;
      }
      const heading = /^#{1,4}\s+(.*)$/.exec(line);
      if (heading) {
        flushParagraph();
        flushList();
        nodes.push(h("p", { class: "chat-heading" }, ...inline(heading[1])));
        continue;
      }
      if (list) list.items[list.items.length - 1] += ` ${line.trim()}`;
      else paragraph.push(line.trim());
    }
    flushParagraph();
    flushList();
    return nodes;
  }

  private codeBlock(code: string, language: string): HTMLElement {
    const insertable = INSERTABLE.has(language);
    const bar = h("div", { class: "chat-code-bar" }, h("span", { class: "chat-code-lang" }, language || "code"), spacer());
    if (insertable) {
      bar.append(
        codeButton("edit", "Insert", "Insert into the active query editor", () => this.host.insertSql(code)),
        codeButton("query", "New tab", "Open in a new query tab", () => this.host.openQuery(code)),
      );
    }
    bar.append(codeButton("copy", "Copy", "Copy to clipboard", () => void copyText(code).then(() => toast("Copied"))));
    return h("div", { class: "chat-code" }, bar, h("pre", {}, h("code", {}, code)));
  }
}

function codeButton(iconName: IconName, label: string, title: string, run: () => void): HTMLElement {
  const button = h("button", { class: "chat-code-button", type: "button", title }, icon(iconName), label);
  button.addEventListener("click", run);
  return button;
}

/** Inline markdown: `code`, **bold**. Everything else is text. */
function inline(text: string): (HTMLElement | string)[] {
  const parts: (HTMLElement | string)[] = [];
  const pattern = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let last = 0;
  for (const match of text.matchAll(pattern)) {
    const start = match.index ?? 0;
    if (start > last) parts.push(text.slice(last, start));
    const token = match[0];
    if (token.startsWith("`")) parts.push(h("code", {}, token.slice(1, -1)));
    else parts.push(h("strong", {}, token.slice(2, -2)));
    last = start + token.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

function fence(code: string, language: string): string {
  return `\`\`\`${language}\n${code.trim()}\n\`\`\``;
}

function contextChip(context: AssistantContext): HTMLElement[] {
  if (!context.connectionId) {
    return [h("span", { class: "assistant-context-text muted" }, icon("plug"), "Not connected. Connect to get answers based on your schema.")];
  }
  const info = driverInfo(context.driver);
  const parts = [context.connectionName ?? "connection", context.database, context.table].filter(Boolean).join(" · ");
  return [h("span", { class: "assistant-context-text", title: context.serverVersion ?? "" }, icon(info.icon, `driver-icon ${info.id}`), parts)];
}

function chatContext(context: AssistantContext): ChatContext {
  return {
    connectionId: context.connectionId,
    connectionName: context.connectionName,
    driverLabel: context.driver ? driverInfo(context.driver).label : undefined,
    serverVersion: context.serverVersion,
    database: context.database,
    table: context.table,
    editorText: context.editorText?.() || undefined,
    lastError: context.lastError?.() ?? undefined,
    extra: context.extra?.() || undefined,
  };
}
