/**
 * The assistant panel: a chat docked on the right of the workspace. Every code block the model writes carries "Run"
 * (the user's own run through the workspace rules, results under the block), "Insert" and "New tab". Where the user
 * allows it, the model also runs read-only queries itself; each shows as an expandable tool call. What the panel offers
 * per engine (code-fence tags, quick prompts, translation targets) comes from the engine descriptors; an engine module may
 * add buttons of its own for fence tags it applies itself (`core/assistantFences.ts`: a MongoDB filter for the query bar).
 */
import { api, errorText } from "../../core/api";
import { fenceActionsOf, type FenceAction } from "../../core/assistantFences";
import { capabilities } from "../../core/capabilities";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton, iconOrDefault, spacer, type IconName } from "../../core/dom";
import { capabilityOf, defaultLanguage, engineInfo, isKnownFence, languageForFence, visibleEngines, type EngineInfo } from "../../core/engines";
import { ChatRun, queryToolCall } from "./chatRun";
import { conversationMove, objectWords, parseMarkdown, thinkingAfter, topicKey, type Align, type ChatTopic } from "./chatText";
import { ModelSwitcher } from "./modelSwitcher";
import type { ObjectRef } from "../../core/contract";
import type { ProfileStatus } from "../../core/profileTypes";
import { staleBadge } from "./profileView";
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
  /** The object of the active workspace tab (a collection), for fences that apply to it. */
  object?: ObjectRef | null;
  /** Text of the active query editor (selection or all). */
  editorText?: () => string;
  lastError?: () => string | null;
  /** Free-form extra context, e.g. "Command kubectl … Last log lines: …". */
  extra?: () => string;
  /** Documents sampled in the view, already cut; the backend sends them only where reads are allowed. */
  samples?: () => string | null;
}

export interface AssistantHost {
  context(): AssistantContext;
  /** Puts `text` into the active query editor, or opens a query tab for it. */
  insertSql(text: string): void;
  openQuery(text: string): void;
  /** Opens (or selects) an object's workspace on `section` and hands the section `request`. */
  openWorkspace(connectionId: string, object: ObjectRef, section: string, request: unknown): void;
  /** Opens what the model asked for (diagram, table, query tab) on the current connection. */
  perform(action: UiAction): void;
  /** What clicking a suggested app action (`suggestAppAction`) runs; null for an id this version does not know. */
  appAction(id: string): (() => void) | null;
  /** Opens the LLM provider dialog for the active connection (or a new one). */
  configure(): void;
  /** Opens the LLM provider dialog for `connection`, or an empty one to add a provider. */
  editProvider(connection: AiConnection | null): void;
  configChanged(config: AppConfig): void;
  close(): void;
  /** Opens "Learn this database" for `database` of `connectionId` (a refresh when a profile exists). */
  learn(connectionId: string, database: string): void;
  /** Opens the "What the assistant knows" tab. */
  openKnowledge(connectionId: string, database: string): void;
  /** Whether the settings let the assistant learn databases on `connectionId`. */
  canLearn(connectionId: string): boolean;
}

interface QuickAction {
  label: string;
  icon: IconName;
  /** `send` goes straight to the model; `prefill` puts a template in the composer; `translate` opens the target menu. */
  mode: "send" | "prefill" | "translate";
  prompt: () => string;
}

/** One chat: its messages (sent as history) and its log. A conversation belongs to one connection and database. */
interface Conversation {
  messages: ChatMessage[];
  log: HTMLElement;
  /** What it is about; null until the chat first has a connection. */
  topic: ChatTopic | null;
}

const MAX_HISTORY = 30;

function newConversation(): Conversation {
  return { messages: [], log: h("div", { class: "chat-log" }), topic: null };
}

export class AssistantPanel {
  readonly element: HTMLElement;
  private readonly host: AssistantHost;
  private readonly modelBadge = h("span", { class: "assistant-model" });
  private readonly contextRow = h("div", { class: "assistant-context" });
  private readonly logHost = h("div", { class: "chat-log-host" });
  private readonly actions = h("div", { class: "chat-actions" });
  private readonly composer = h("textarea", {
    class: "chat-input",
    rows: "1",
    placeholder: "Ask about your data, or describe the query you need…",
    spellcheck: "false",
  });
  private readonly sendButton: HTMLButtonElement;
  private readonly stopButton: HTMLButtonElement;
  private readonly setup: HTMLElement;
  private readonly chat: HTMLElement;
  /** The conversations of each workspace by topic ("scope\u0000connection|database"); switching keeps the others. */
  private readonly conversations = new Map<string, Conversation>();
  /** The conversation last open in each workspace. */
  private readonly openByScope = new Map<string, Conversation>();
  private scope = "";
  private conversation: Conversation = newConversation();
  private readonly switcher: ModelSwitcher;
  private configured = false;
  private activeRequest: string | null = null;
  private readonly learnButton: HTMLButtonElement;
  /** The profile status of the chat's connection and database (`key`), fetched when they change. */
  private knowledge: { key: string; status: ProfileStatus | null } = { key: "", status: null };

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

    this.learnButton = iconButton("book", "Learn this database", () => {
      const context = this.host.context();
      if (context.connectionId) this.host.learn(context.connectionId, context.database ?? "");
    });
    const header = h(
      "div",
      { class: "assistant-header" },
      icon("sparkle", "assistant-icon"),
      h("span", { class: "assistant-title" }, "Assistant"),
      this.modelBadge,
      spacer(),
      this.learnButton,
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
      h(
        "p",
        {},
        `Connect Fosta, OpenAI, Gemini, Claude (Anthropic), Ollama or any OpenAI-compatible endpoint with your key. The key stays in ${capabilities().secretStore}; requests go straight from ${capabilities().edition === "web" ? "the Fosta Studio server" : "this computer"} to the provider.`,
      ),
      h(
        "p",
        {},
        "The assistant sees table and column names, your editor text and errors. It writes queries you run with one click; where you allow it (Settings), it also runs read-only queries itself, and the rows it reads are sent to your provider.",
      ),
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

  /** Conversations per workspace; switching keeps the others. */
  setScope(scope: string): void {
    this.scope = scope;
    this.show(this.openByScope.get(scope) ?? newConversation());
    this.refreshContext();
  }

  private show(conversation: Conversation): void {
    this.conversation = conversation;
    this.openByScope.set(this.scope, conversation);
    this.logHost.replaceChildren(conversation.log);
    this.scrollToEnd();
  }

  /**
   * Opens the conversation of the context's connection and database (kept from before, or a new one), so a chat never
   * sends another database's history. Not while an answer streams: the panel follows once it is done.
   */
  private followContext(context: AssistantContext): void {
    if (this.activeRequest) return;
    const move = conversationMove(this.conversation.topic, context);
    if (move.kind === "stay") return;
    const key = `${this.scope}\u0000${topicKey(move.topic)}`;
    if (move.kind === "adopt") {
      // A chat without a topic yet becomes this topic's, unless the topic already has one.
      const existing = this.conversations.get(key);
      if (existing && existing !== this.conversation) this.show(existing);
      else {
        this.conversation.topic = move.topic;
        this.conversations.set(key, this.conversation);
      }
      return;
    }
    let next = this.conversations.get(key);
    if (!next) {
      next = { ...newConversation(), topic: move.topic };
      this.conversations.set(key, next);
    }
    if (next !== this.conversation) this.show(next);
  }

  /** Re-reads the host context: chip under the header, quick actions, and the conversation it belongs to. */
  refreshContext(): void {
    const context = this.host.context();
    this.followContext(context);
    const key = context.connectionId ? `${context.connectionId}|${context.database ?? ""}` : "";
    if (key !== this.knowledge.key) {
      this.knowledge = { key, status: null };
      if (context.connectionId) void this.loadKnowledge(context.connectionId, context.database ?? "", false);
    }
    this.renderContext(context);
  }

  /** Re-reads what the assistant knows about the chat's database (after learning, deleting or editing it). */
  knowledgeChanged(connectionId: string, database: string): void {
    if (this.knowledge.key === `${connectionId}|${database}`) void this.loadKnowledge(connectionId, database, true);
  }

  /** A status read elsewhere ("What the assistant knows") for the chat's database: shown without asking again. */
  setKnowledge(connectionId: string, database: string, status: ProfileStatus): void {
    const key = `${connectionId}|${database}`;
    if (this.knowledge.key !== key) return;
    this.knowledge = { key, status };
    this.renderContext(this.host.context());
  }

  /** Checks again whether the chat's database changed (the panel was opened; the backend caches the answer). */
  recheckKnowledge(): void {
    const context = this.host.context();
    if (context.connectionId) void this.loadKnowledge(context.connectionId, context.database ?? "", false);
  }

  private async loadKnowledge(connectionId: string, database: string, force: boolean): Promise<void> {
    const key = `${connectionId}|${database}`;
    let status: ProfileStatus | null = null;
    try {
      status = await api.profileStatus(connectionId, database, force);
    } catch {
      // Unknown: no badge and no chip.
    }
    if (this.knowledge.key !== key) return;
    this.knowledge = { key, status };
    this.renderContext(this.host.context());
  }

  private renderContext(context: AssistantContext): void {
    const connectionId = context.connectionId;
    // An engine with several databases learns one of them: the chat needs one picked.
    const needsDatabase = Boolean(connectionId) && !context.database && capabilityOf(connectionId!, "multipleDatabases");
    const allowed = connectionId ? this.host.canLearn(connectionId) && !needsDatabase : false;
    this.learnButton.disabled = !connectionId || !allowed;
    this.learnButton.title = !connectionId
      ? "Learn this database (connect first)"
      : needsDatabase
        ? "Learn this database (select a database in the tree first)"
        : allowed
          ? this.knowledge.status?.exists
            ? "Learn this database again (refresh what the assistant knows)"
            : "Learn this database: save its structure so the assistant knows it in every chat"
          : "Learning databases is turned off (Settings → Assistant may learn databases)";
    const status = this.knowledge.status;
    const database = context.database ?? "";
    const knows: HTMLElement[] = [];
    if (connectionId && status?.exists) {
      const link = h(
        "button",
        { class: "assistant-knows", type: "button", title: "What the assistant knows about this database" },
        icon("book"),
        `Knows ${status.objects} objects`,
      );
      link.addEventListener("click", () => this.host.openKnowledge(connectionId, database));
      knows.push(link);
      if (status.stale && allowed) knows.push(staleBadge(() => this.host.learn(connectionId, database)));
    }
    this.contextRow.replaceChildren(...contextChip(context), ...(knows.length ? [spacer(), ...knows] : []));
    const chips = this.quickActions(context).map((action) => this.actionChip(action));
    // A new chat on a database the assistant has not learned: offer to learn it first.
    if (connectionId && allowed && status && !status.exists && this.conversation.messages.length === 0) {
      const learn = h(
        "button",
        { class: "chat-chip learn-chip", type: "button", title: "Save the structure so the assistant knows it in every chat" },
        icon("book"),
        "Learn this database",
      );
      learn.addEventListener("click", () => this.host.learn(connectionId, database));
      chips.unshift(learn);
    }
    this.actions.replaceChildren(...chips);
    this.actions.hidden = this.actions.childElementCount === 0;
  }

  focus(): void {
    this.composer.focus();
  }

  /**
   * Sends a prepared question, e.g. from an "Ask assistant" button elsewhere.
   * `extra` is added to the context of this message only (a task log, say),
   * so the answer does not depend on which tab happens to be active; `samples` likewise (sampled documents). `shown`
   * is what the chat shows as the user's message when `prompt` carries instructions for the model (a query bar's "Ask").
   */
  ask(prompt: string, extra?: string, samples?: string, shown?: string): void {
    this.composer.value = prompt;
    void this.send(extra, samples, shown);
  }

  private reset(): void {
    this.stop();
    this.conversation.messages = [];
    this.conversation.log.replaceChildren();
    this.renderContext(this.host.context());
  }

  private async send(extra?: string, samples?: string, shown?: string): Promise<void> {
    const text = this.composer.value.trim();
    if (!text || this.activeRequest || !this.configured) return;
    this.composer.value = "";
    this.growComposer();
    const current = this.host.context();
    // The question goes to the conversation of the connection and database it is about.
    this.followContext(current);
    const conversation = this.conversation;
    let context: AssistantContext = extra ? { ...current, extra: () => extra } : current;
    if (samples !== undefined) context = { ...context, samples: () => samples };
    this.append({ role: "user", content: text }, shown?.trim() || undefined);
    // The "Learn this database" chip is for new chats only.
    this.actions.querySelector(".learn-chip")?.remove();
    this.actions.hidden = this.actions.childElementCount === 0;
    const bubble = this.bubble("assistant");
    const body = bubble.querySelector<HTMLElement>(".chat-body")!;
    const id = crypto.randomUUID();
    this.activeRequest = id;
    this.setBusy(true);
    let reply = "";
    let pendingTool: HTMLElement | null = null;
    let suggestions: HTMLElement | null = null;
    // Between tool rounds the model is working again: a "Thinking…" row under the tool calls until it writes.
    const thinking = h("div", { class: "chat-thinking", role: "status" }, h("span", { class: "chat-run-spinner" }), "Thinking…");
    const think = (on: boolean | null) => {
      if (on) {
        bubble.before(thinking);
        this.scrollToEnd();
      } else if (on === false) thinking.remove();
    };
    // Code blocks run against the connection the question was about, even if the user moves on.
    const render = () => body.replaceChildren(...this.renderMarkdown(reply, context));
    body.append(h("span", { class: "chat-cursor" }));
    try {
      const request = { id, messages: conversation.messages.slice(-MAX_HISTORY), context: chatContext(context) };
      const full = await api.aiChat(request, (event: ChatEvent) => {
        if (event.type === "text") {
          reply += event.text;
          render();
        } else if (event.type === "tool") {
          pendingTool = h("div", { class: "chat-tool" }, icon("search"), event.label);
          bubble.before(pendingTool);
          this.scrollToEnd();
        } else if (event.type === "query") {
          // The result of the tool call announced just before; the model now works on it.
          const call = queryToolCall(event);
          if (pendingTool) pendingTool.replaceWith(call);
          else bubble.before(call);
          pendingTool = null;
        } else if (event.type === "action" && event.action.kind === "suggestAppAction") {
          // Only a button: nothing runs until the user clicks it.
          suggestions ??= h("div", { class: "chat-app-actions" });
          if (!suggestions.isConnected) bubble.after(suggestions);
          suggestions.append(appActionButton(event.action, this.host.appAction(event.action.id)));
          this.scrollToEnd();
        } else if (event.type === "action") {
          this.host.perform(event.action);
        }
        think(thinkingAfter(event));
      });
      reply = full || reply;
      render();
      conversation.messages.push({ role: "assistant", content: reply });
    } catch (error) {
      const message = errorText(error);
      if (reply) conversation.messages.push({ role: "assistant", content: reply });
      render();
      body.append(h("div", { class: "chat-error" }, message === "Stopped" ? "Stopped." : message));
    } finally {
      think(false);
      this.activeRequest = null;
      this.setBusy(false);
      this.scrollToEnd();
      // The context may have moved on while the answer streamed.
      this.refreshContext();
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

  /** Adds a message to the conversation; the chat shows `shown` instead of its content when given. */
  private append(message: ChatMessage, shown?: string): void {
    this.conversation.messages.push(message);
    const bubble = this.bubble(message.role);
    bubble.querySelector(".chat-body")!.append(...this.renderMarkdown(shown ?? message.content, this.host.context()));
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
    const info = engineInfo(context.driver);
    const sql = () => fence(context.editorText?.() ?? "", defaultLanguage(info)?.fences[0] ?? "");
    const table = context.table ?? "this table";
    const prompts = enginePrompts(info);
    switch (context.kind) {
      case "query": {
        const actions: QuickAction[] = [
          { label: "Explain", icon: "view", mode: "send", prompt: () => `Explain what this does, step by step:\n${sql()}` },
          {
            label: "Optimize",
            icon: "bolt",
            mode: "send",
            prompt: () => `Suggest how to make this faster (indexes, rewrites) and show the improved version:\n${sql()}`,
          },
        ];
        if (context.lastError?.()) {
          actions.unshift({
            label: "Fix error",
            icon: "health",
            mode: "send",
            // The server's error text stays out of the user's message: it reaches the model as untrusted context data
            // ("Last error from the server"), so text planted in an error cannot speak with the user's voice.
            prompt: () => `This failed with the last error from the server (in the context):\n\n${sql()}\nFix it and explain the cause in one sentence.`,
          });
        }
        actions.push(...prompts);
        if (info.assistant.translateFrom) actions.push({ label: "Translate to…", icon: "globe", mode: "translate", prompt: () => "" });
        return actions;
      }
      case "table": {
        const words = objectWords(context.object?.kind);
        return [
          {
            label: "Describe",
            icon: "design",
            mode: "send",
            prompt: () => `Describe ${table}: ${words.describe}.`,
          },
          {
            label: "Test data",
            icon: "table",
            mode: "send",
            prompt: () => `Generate 20 ${words.records} of realistic test data for ${table} as ${info.assistant.testData}.`,
          },
          { label: `Query this ${words.object}`, icon: "query", mode: "prefill", prompt: () => `Write a query on ${table} that ` },
        ];
      }
      case "task":
        return [
          {
            label: "Why does it fail?",
            icon: "health",
            mode: "send",
            prompt: () => "This command is not working. Read the log, explain what is wrong and how to fix it.",
          },
          { label: "Explain the log", icon: "logs", mode: "send", prompt: () => "Explain what this log says, briefly." },
        ];
      case "connection":
        return [{ label: "Write a query", icon: "query", mode: "prefill", prompt: () => "Write a query that " }, ...prompts];
      default:
        return [];
    }
  }

  private actionChip(action: QuickAction): HTMLElement {
    const chip = h("button", { class: "chat-chip", type: "button" }, icon(action.icon), action.label);
    chip.addEventListener("click", (event) => {
      if (action.mode === "translate") {
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
    const source = engineInfo(context.driver);
    return visibleEngines()
      .filter((target) => target.id !== source.id && target.assistant.translateTo !== null)
      .map((target) => ({
        label: target.assistant.translateLabel ?? target.label,
        icon: iconOrDefault(target.icon, "database"),
        run: () => {
          const code = fence(context.editorText?.() ?? "", defaultLanguage(source)?.fences[0] ?? "");
          this.ask(`Translate this to ${target.assistant.translateTo}, keeping the same result. Mention anything without a direct equivalent:\n${code}`);
        },
      }));
  }

  // --- Markdown-lite -----------------------------------------------------

  private renderMarkdown(text: string, context: AssistantContext): HTMLElement[] {
    return parseMarkdown(text).map((block) => {
      switch (block.type) {
        case "code":
          return this.codeBlock(block.code, block.language, context);
        case "heading":
          return h("p", { class: "chat-heading" }, ...inline(block.text));
        case "list":
          return h(block.ordered ? "ol" : "ul", {}, ...block.items.map((item) => h("li", {}, ...inline(item))));
        case "table":
          return markdownTable(block.header, block.align, block.rows);
        default:
          return h("p", {}, ...inline(block.text));
      }
    });
  }

  /**
   * A code block. Run appears when the block's fence names a language of the open connection (an untagged block runs in
   * the default language); Insert and New tab for any fence an engine of this build knows.
   */
  private codeBlock(code: string, fenceTag: string, context: AssistantContext): HTMLElement {
    const bar = h("div", { class: "chat-code-bar" }, h("span", { class: "chat-code-lang" }, fenceTag || "code"), spacer());
    const block = h("div", { class: "chat-code" }, bar, h("pre", {}, h("code", {}, code)));
    const connectionId = context.connectionId;
    const language = connectionId && capabilityOf(connectionId, "query") ? languageForFence(engineInfo(context.driver), fenceTag) : null;
    if (connectionId && language && code.trim()) {
      const run = new ChatRun(
        code,
        { connectionId, database: context.database ?? null, language },
        {
          openQuery: (text) => this.host.openQuery(text),
          resized: () => this.scrollToEnd(),
        },
      );
      block.append(run.element);
      bar.append(codeButton("play", "Run", `Run on ${context.connectionName ?? "the connection"} (the workspace rules apply)`, () => void run.run()));
    }
    if (isKnownFence(fenceTag)) {
      bar.append(
        codeButton("edit", "Insert", "Insert into the active query editor", () => this.host.insertSql(code)),
        codeButton("query", "New tab", "Open in a new query tab", () => this.host.openQuery(code)),
      );
    }
    const copy = codeButton("copy", "Copy", "Copy to clipboard", () => void copyText(code).then(() => toast("Copied")));
    bar.append(copy);
    if (connectionId && code.trim()) {
      // The engine's own buttons for this fence (a filter for the query bar, a pipeline for Aggregations).
      const problem = h("div", { class: "chat-code-error", role: "alert" });
      problem.hidden = true;
      block.append(problem);
      const target = {
        connectionId,
        database: context.database ?? null,
        object: context.object ?? null,
        openWorkspace: (object: ObjectRef, section: string, request: unknown) => this.host.openWorkspace(connectionId, object, section, request),
      };
      void fenceActionsOf(context.driver, fenceTag).then((actions) => {
        const made = actions?.(code, target) ?? [];
        for (const action of made) copy.before(fenceButton(action, problem));
        // A block that writes or reaches another database says so under it; its buttons ask before applying.
        const warning = made.find((action) => action.warning)?.warning;
        if (warning) problem.before(h("div", { class: "chat-code-warning", role: "note" }, icon("shield"), h("span", {}, warning)));
      });
    }
    return block;
  }
}

/** An engine's button for a fence; what it reports (a parse error) shows under the block instead of applying. */
function fenceButton(action: FenceAction, problem: HTMLElement): HTMLElement {
  return codeButton(action.icon, action.label, action.title, () => {
    void Promise.resolve()
      .then(() => action.run())
      .then(
        (message) => {
          problem.textContent = message ?? "";
          problem.hidden = !message;
        },
        (error: unknown) => {
          problem.textContent = errorText(error);
          problem.hidden = false;
        },
      );
  });
}

function codeButton(iconName: IconName, label: string, title: string, run: () => void): HTMLElement {
  const button = h("button", { class: "chat-code-button", type: "button", title }, icon(iconName), label);
  button.addEventListener("click", run);
  return button;
}

/**
 * A suggested app action: a button that runs `run` on click (never before), with a hint that it is the app's own action.
 * An id this version does not know shows a note instead.
 */
export function appActionButton(action: { id: string; label: string }, run: (() => void) | null): HTMLElement {
  if (!run) return h("div", { class: "chat-app-action-unknown" }, `The assistant suggested an app action this version does not have (${action.id}).`);
  const button = h(
    "button",
    { class: "chat-app-action", type: "button", title: `Runs only when you click (${action.id})` },
    icon("sparkle"),
    h("span", { class: "chat-app-action-label" }, action.label),
    h("span", { class: "chat-app-action-hint" }, "Fosta Studio action"),
  );
  button.addEventListener("click", () => run());
  return button;
}

/** A GFM table of the answer; cells are text with inline markdown, never HTML. */
function markdownTable(header: string[], align: Align[], rows: string[][]): HTMLElement {
  const cell = (tag: "th" | "td", text: string, column: number) => h(tag, align[column] ? { class: `align-${align[column]}` } : {}, ...inline(text));
  return h(
    "div",
    { class: "chat-md-table" },
    h(
      "table",
      {},
      h("thead", {}, h("tr", {}, ...header.map((text, column) => cell("th", text, column)))),
      h("tbody", {}, ...rows.map((row) => h("tr", {}, ...row.map((text, column) => cell("td", text, column))))),
    ),
  );
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

/** The engine's own quick prompts (prefilled in the composer). */
function enginePrompts(info: EngineInfo): QuickAction[] {
  return info.assistant.prompts.map((prompt) => ({
    label: prompt.label,
    icon: iconOrDefault(prompt.icon, "sparkle"),
    mode: "prefill",
    prompt: () => prompt.prompt,
  }));
}

/** A fenced block whose fence is longer than any backtick run in `code`, so the code cannot close it. */
function fence(code: string, language: string): string {
  const longest = Math.max(0, ...(code.match(/`+/g) ?? []).map((run) => run.length));
  const ticks = "`".repeat(Math.max(3, longest + 1));
  return `${ticks}${language}\n${code.trim()}\n${ticks}`;
}

function contextChip(context: AssistantContext): HTMLElement[] {
  if (!context.connectionId) {
    return [h("span", { class: "assistant-context-text muted" }, icon("plug"), "Not connected. Connect to get answers based on your schema.")];
  }
  const info = engineInfo(context.driver);
  const parts = [context.connectionName ?? "connection", context.database, context.table].filter(Boolean).join(" · ");
  return [
    h(
      "span",
      { class: "assistant-context-text", title: context.serverVersion ?? "" },
      icon(iconOrDefault(info.icon, "database"), `driver-icon ${info.id}`),
      parts,
    ),
  ];
}

function chatContext(context: AssistantContext): ChatContext {
  return {
    connectionId: context.connectionId,
    connectionName: context.connectionName,
    driverLabel: context.driver ? engineInfo(context.driver).label : undefined,
    serverVersion: context.serverVersion,
    database: context.database,
    table: context.table,
    editorText: context.editorText?.() || undefined,
    lastError: context.lastError?.() ?? undefined,
    extra: context.extra?.() || undefined,
    sampledDocuments: context.samples?.() || undefined,
  };
}
