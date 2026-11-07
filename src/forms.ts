import { api } from "./api";
import { checkbox, field, openModal, textInput, toast } from "./dialogs";
import { h, icon } from "./dom";
import { describeServer, driverInfo, DRIVERS, type DriverInfo } from "./drivers";
import { formatArgs, parseArgs } from "./shell";
import type { AiProvider, AiSettings, AppConfig, ConnectionProfile, EmbeddingAuth, EmbeddingSettings, TaskDef, WorkspaceMeta } from "./types";
import { WORKSPACE_COLORS, WORKSPACE_ICONS, workspaceColor, workspaceIcon } from "./workspaces";

const AUTH_OPTIONS: { value: EmbeddingAuth; label: string }[] = [
  { value: "bearer", label: "Authorization: Bearer <key> (OpenAI, vLLM, most gateways)" },
  { value: "api-key", label: "api-key: <key> (Azure OpenAI)" },
  { value: "none", label: "No key (local Ollama, LM Studio)" },
];

const PORT_FORWARD_TEMPLATE = {
  name: "MariaDB port-forward",
  program: "kubectl",
  args: "port-forward -n default svc/mariadb 13306:3306",
  readyPattern: "Forwarding from",
};

export function connectionDialog(existing: ConnectionProfile | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    let driver = driverInfo(existing?.driver);
    let previousDriver = driver;
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Staging replica" });
    const host = textInput(existing?.host ?? "127.0.0.1");
    const port = textInput(String(existing?.port ?? driver.defaultPort), { type: "number", min: 1, max: 65535 });
    const user = textInput(existing?.user ?? driver.defaultUser);
    const password = textInput("", {
      type: "password",
      placeholder: existing?.savePassword ? "Saved, leave empty to keep" : "",
    });
    const database = textInput(existing?.database ?? "", { placeholder: driver.databaseHint });
    const databaseField = field(driver.databaseLabel, database);
    const remember = checkbox("Save password in Windows Credential Manager", existing?.savePassword ?? true);
    const optionsHost = h("div", { class: "form-grid driver-options" });
    const availability = h("p", { class: "driver-note" });
    const options: Record<string, string> = { ...(existing?.options ?? {}) };
    const optionInputs = new Map<string, HTMLInputElement>();

    const renderOptions = () => {
      optionInputs.clear();
      const controls = driver.options.map((option) => {
        if (option.kind === "checkbox") {
          const stored = options[option.key];
          const box = checkbox(option.label, stored === undefined ? option.default : stored === "true");
          optionInputs.set(option.key, box.input);
          return box.element;
        }
        const input = textInput(options[option.key] ?? "", { class: "input mono", placeholder: option.placeholder ?? "" });
        optionInputs.set(option.key, input);
        return field(option.label, input, option.hint ?? "");
      });
      optionsHost.replaceChildren(...controls);
      optionsHost.hidden = controls.length === 0;
      availability.textContent = driver.available ? "" : `${driver.label} support is in progress: the profile can be saved now, connecting comes in a later build.`;
      availability.hidden = driver.available;
    };

    const selectDriver = (next: DriverInfo) => {
      for (const [key, input] of optionInputs) {
        options[key] = input.type === "checkbox" ? String(input.checked) : input.value;
      }
      driver = next;
      if (Number(port.value) === previousDriver.defaultPort) port.value = String(next.defaultPort);
      if (user.value.trim() === previousDriver.defaultUser) user.value = next.defaultUser;
      previousDriver = next;
      database.placeholder = next.databaseHint;
      databaseField.querySelector(".field-label")!.textContent = next.databaseLabel;
      for (const card of cards) card.classList.toggle("active", card.dataset.driver === next.id);
      renderOptions();
    };

    const cards = DRIVERS.map((info) => {
      const card = h(
        "button",
        { class: `driver-card${info.id === driver.id ? " active" : ""}`, type: "button", "data-driver": info.id, title: info.label },
        icon(info.icon, `driver-icon ${info.id}`),
        h("span", { class: "driver-label" }, info.label),
        info.available ? null : h("span", { class: "driver-soon" }, "soon"),
      );
      card.addEventListener("click", () => selectDriver(info));
      return card;
    });
    renderOptions();

    const readOptions = (): Record<string, string> => {
      const result: Record<string, string> = {};
      for (const [key, input] of optionInputs) {
        // Checkboxes are always stored so an unticked default-on option (TLS) stays off.
        if (input.type === "checkbox") result[key] = String(input.checked);
        else if (input.value.trim() !== "") result[key] = input.value.trim();
      }
      return result;
    };
    const readProfile = (): ConnectionProfile => ({
      id,
      name: name.value.trim() || `${host.value.trim()}:${port.value}`,
      driver: driver.id,
      host: host.value.trim(),
      port: Number(port.value) || driver.defaultPort,
      user: user.value.trim(),
      database: database.value.trim() || null,
      savePassword: remember.input.checked,
      options: readOptions(),
    });
    const readPassword = (): string | null => (password.value === "" && existing?.savePassword ? null : password.value);

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Saved in workspace ", h("strong", {}, workspaceName), "."),
      field("Database type", h("div", { class: "driver-cards" }, ...cards)),
      availability,
      field("Connection name", name),
      h("div", { class: "field-row" }, field("Host", host), h("div", { class: "narrow" }, field("Port", port))),
      h("div", { class: "field-row even" }, field("User", user), field("Password", password)),
      databaseField,
      optionsHost,
      remember.element,
    );
    openModal(existing ? "Edit connection" : "New connection", body, [
      {
        label: "Test connection",
        alignLeft: true,
        run: async () => {
          const profile = readProfile();
          const version = await api.testConnection(profile, readPassword());
          toast(`Connection OK · ${describeServer(profile, version)}`, "success");
          return false;
        },
      },
      { label: "Cancel", run: () => undefined },
      { label: "Save", primary: true, run: async () => void (saved = await api.saveConnection(readProfile(), readPassword())) },
    ], { width: 520, onClose: () => resolve(saved) });
  });
}

export function workspaceDialog(existing: WorkspaceMeta | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Production, Client X, Local" });
    let color = workspaceColor(existing?.color ?? WORKSPACE_COLORS[Math.floor(Math.random() * WORKSPACE_COLORS.length)]);
    let iconName = workspaceIcon(existing?.icon ?? "laptop");
    const preview = h("div", { class: `ws-preview color-${color}` }, icon(iconName));
    const refreshPreview = () => {
      preview.className = `ws-preview color-${color}`;
      preview.replaceChildren(icon(iconName));
    };
    const swatches = WORKSPACE_COLORS.map((option) => {
      const swatch = h("button", { class: `swatch color-${option}${option === color ? " active" : ""}`, type: "button", title: option, "aria-label": option });
      swatch.addEventListener("click", () => {
        color = option;
        for (const item of swatches) item.classList.toggle("active", item === swatch);
        refreshPreview();
      });
      return swatch;
    });
    const icons = WORKSPACE_ICONS.map((option) => {
      const button = h("button", { class: `icon-choice${option.name === iconName ? " active" : ""}`, type: "button", title: option.label, "aria-label": option.label }, icon(option.name), h("span", {}, option.label));
      button.addEventListener("click", () => {
        iconName = option.name;
        for (const item of icons) item.classList.toggle("active", item === button);
        refreshPreview();
      });
      return button;
    });

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "A workspace keeps its own connections and startup tasks. Switch between them from the rail on the left."),
      h("div", { class: "ws-preview-row" }, preview, field("Workspace name", name)),
      field("Kind", h("div", { class: "icon-choices" }, ...icons)),
      field("Color", h("div", { class: "swatches" }, ...swatches)),
    );
    openModal(existing ? "Edit workspace" : "New workspace", body, [
      { label: "Cancel", run: () => undefined },
      {
        label: existing ? "Save" : "Create",
        primary: true,
        run: async () => {
          const trimmed = name.value.trim();
          if (!trimmed) throw new Error("Workspace name is required");
          saved = await api.saveWorkspace({ id, name: trimmed, color, icon: iconName });
        },
      },
    ], { width: 480, onClose: () => resolve(saved) });
  });
}

export function embeddingDialog(existing: EmbeddingSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const url = textInput(existing?.url ?? "", { class: "input mono", placeholder: "https://api.openai.com/v1/embeddings" });
    const model = textInput(existing?.model ?? "", { class: "input mono", placeholder: "text-embedding-3-large" });
    const auth = h("select", { class: "input" }, ...AUTH_OPTIONS.map((option) => h("option", { value: option.value, selected: option.value === (existing?.auth ?? "bearer") }, option.label)));
    const apiKey = textInput("", { type: "password", placeholder: existing ? "Saved, leave empty to keep" : "" });
    const dimensions = textInput(existing?.dimensions ? String(existing.dimensions) : "", { type: "number", min: 1, placeholder: "optional, e.g. 1024" });

    const readSettings = (): EmbeddingSettings => {
      if (!url.value.trim() || !model.value.trim()) throw new Error("URL and model are required");
      return {
        url: url.value.trim(),
        model: model.value.trim(),
        auth: auth.value as EmbeddingAuth,
        dimensions: Number(dimensions.value) || null,
      };
    };
    const save = async () => {
      saved = await api.saveEmbeddingSettings(readSettings(), apiKey.value || null);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Any OpenAI-compatible /embeddings endpoint. Use the same model that produced the stored vectors, otherwise distances are meaningless."),
      field("Endpoint URL", url),
      field("Model", model),
      field("Authentication", auth),
      field("API key", apiKey, "Stored in Windows Credential Manager"),
      field("Dimensions", dimensions, "Sent as `dimensions` for models that support shortening (OpenAI v3)"),
    );
    openModal("Embedding API", body, [
      { label: "Remove", alignLeft: true, danger: true, run: async () => void (saved = await api.saveEmbeddingSettings(null, null)) },
      { label: "Cancel", run: () => undefined },
      {
        label: "Save & test",
        run: async () => {
          await save();
          toast(`Embedding OK: ${await api.testEmbedding()} dimensions`, "success");
        },
      },
      { label: "Save", primary: true, run: save },
    ], { width: 540, onClose: () => resolve(saved) });
  });
}

export function taskDialog(existing: TaskDef | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "");
    const program = textInput(existing?.program ?? "kubectl", { class: "input mono" });
    const args = textInput(formatArgs(existing?.args ?? []), {
      class: "input mono",
      placeholder: "port-forward -n <namespace> svc/<service> <local>:<remote>",
    });
    const readyPattern = textInput(existing?.readyPattern ?? "", { placeholder: "text in output that means ready" });
    const autoStart = checkbox("Start automatically when this workspace opens", existing?.autoStart ?? true);
    const autoRestart = checkbox("Restart automatically when the process exits", existing?.autoRestart ?? true);
    const useTemplate = h("button", { class: "link-button", type: "button" }, "Fill kubectl port-forward template");
    useTemplate.addEventListener("click", () => {
      name.value ||= PORT_FORWARD_TEMPLATE.name;
      program.value = PORT_FORWARD_TEMPLATE.program;
      args.value = PORT_FORWARD_TEMPLATE.args;
      readyPattern.value = PORT_FORWARD_TEMPLATE.readyPattern;
    });

    const readTask = (): TaskDef => {
      const command = program.value.trim();
      if (!command) throw new Error("Program is required");
      return {
        id,
        name: name.value.trim() || command,
        program: command,
        args: parseArgs(args.value),
        autoStart: autoStart.input.checked,
        autoRestart: autoRestart.input.checked,
        readyPattern: readyPattern.value.trim() || null,
      };
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Runs in the background while workspace ", h("strong", {}, workspaceName), " is open. ", useTemplate),
      field("Task name", name),
      field("Program", program, "Must be on PATH, e.g. kubectl"),
      field("Arguments", args),
      field("Ready when output contains", readyPattern),
      autoStart.element,
      autoRestart.element,
    );
    openModal(existing ? "Edit startup task" : "New startup task", body, [
      { label: "Cancel", run: () => undefined },
      { label: "Save", primary: true, run: async () => void (saved = await api.saveTask(readTask())) },
    ], { width: 520, onClose: () => resolve(saved) });
  });
}

interface ProviderPreset {
  value: AiProvider;
  label: string;
  url: string;
  model: string;
  hint: string;
}

const AI_PROVIDERS: ProviderPreset[] = [
  { value: "anthropic", label: "Anthropic (Claude)", url: "https://api.anthropic.com", model: "claude-opus-5-5", hint: "Key from console.anthropic.com" },
  { value: "openai", label: "OpenAI-compatible (OpenAI, Ollama, OpenRouter, DeepSeek…)", url: "https://api.openai.com/v1", model: "", hint: "Ollama: http://localhost:11434/v1 with any model you pulled, no key needed" },
];

export function aiDialog(existing: AiSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const presetOf = (value: string) => AI_PROVIDERS.find((preset) => preset.value === value) ?? AI_PROVIDERS[0];
    let preset = presetOf(existing?.provider ?? "anthropic");
    const provider = h("select", { class: "input" }, ...AI_PROVIDERS.map((item) => h("option", { value: item.value, selected: item.value === preset.value }, item.label)));
    const baseUrl = textInput(existing?.baseUrl ?? "", { class: "input mono", placeholder: preset.url });
    const model = textInput(existing?.model ?? "", { class: "input mono", placeholder: preset.model || "e.g. gpt-5, llama3.1, deepseek-chat" });
    const apiKey = textInput("", { type: "password", placeholder: existing ? "Saved, leave empty to keep" : "" });
    const hint = h("span", { class: "field-hint" }, preset.hint);
    provider.addEventListener("change", () => {
      const previous = preset;
      preset = presetOf(provider.value);
      baseUrl.placeholder = preset.url;
      model.placeholder = preset.model || "e.g. gpt-5, llama3.1, deepseek-chat";
      if (!model.value || model.value === previous.model) model.value = preset.model;
      hint.textContent = preset.hint;
    });

    const readSettings = (): AiSettings => {
      if (!model.value.trim()) throw new Error("Model is required");
      return { provider: preset.value, baseUrl: baseUrl.value.trim(), model: model.value.trim() };
    };
    const save = async () => {
      saved = await api.saveAiSettings(readSettings(), apiKey.value || null);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "The assistant calls the provider directly from this computer with your key. It only sees table and column names, your editor text and the errors you ask about; it never runs queries."),
      field("Provider", provider),
      field("Endpoint", baseUrl, "Leave empty for the provider's public endpoint"),
      field("Model", model),
      h("label", { class: "field" }, h("span", { class: "field-label" }, "API key"), apiKey, h("span", { class: "field-hint" }, "Stored in Windows Credential Manager"), hint),
    );
    openModal("Assistant", body, [
      { label: "Remove", alignLeft: true, danger: true, run: async () => void (saved = await api.saveAiSettings(null, null)) },
      { label: "Cancel", run: () => undefined },
      {
        label: "Save & test",
        run: async () => {
          await save();
          toast(`Model replied: ${await api.testAi()}`, "success");
        },
      },
      { label: "Save", primary: true, run: save },
    ], { width: 560, onClose: () => resolve(saved) });
  });
}
