import { capabilities, localhostHint } from "../../core/capabilities";
import { api, errorText } from "../../core/api";
import { checkbox, field, openModal, textInput, toast } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import { describeServer, driverInfo, DRIVERS, type DriverInfo } from "../../core/drivers";
import type { AppConfig, ConnectionProfile } from "../../core/types";
import {
  CONNECTION_STRING_EXAMPLES,
  formatConnectionString,
  MODE_OPTION,
  parseConnectionString,
  type ConnectionFields,
  type ParsedConnection,
} from "./connectionString";

/** The dialog still edits the version-1 option keys; the backend stores them as `url` and `tls`. */
function dialogOptions(profile: ConnectionProfile | null): Record<string, string> {
  const options = { ...(profile?.options ?? {}) };
  if (profile?.driver === "mongodb" && profile.url) options.uri ??= profile.url;
  if (profile?.driver === "mssql" && profile.tls) {
    options.encrypt ??= String(profile.tls.mode !== "disable");
    options.trustServerCertificate ??= String(profile.tls.acceptInvalidCerts);
  }
  return options;
}

export function connectionDialog(existing: ConnectionProfile | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    let driver = driverInfo(existing?.driver);
    let previousDriver = driver;
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Staging replica" });
    const host = textInput(existing?.host ?? (capabilities().inContainer ? "host.docker.internal" : "127.0.0.1"));
    const port = textInput(String(existing?.port ?? driver.defaultPort), { type: "number", min: 1, max: 65535 });
    const user = textInput(existing?.user ?? driver.defaultUser);
    const password = textInput("", {
      type: "password",
      placeholder: existing?.savePassword ? "Saved, leave empty to keep" : "",
    });
    const database = textInput(existing?.database ?? "", { placeholder: driver.databaseHint });
    const databaseField = field(driver.databaseLabel, database);
    const remember = checkbox(`Save password in ${capabilities().secretStore}`, existing?.savePassword ?? true);
    const optionsHost = h("div", { class: "form-grid driver-options" });
    const availability = h("p", { class: "driver-note" });
    const options = dialogOptions(existing);
    const optionInputs = new Map<string, HTMLInputElement>();
    let mode: "params" | "string" = options[MODE_OPTION] === "string" || options.uri ? "string" : "params";
    const connectionString = h("textarea", { class: "input mono connection-string", rows: "3", spellcheck: "false" });
    const stringHint = h("span", { class: "field-hint" });
    /** In the Docker edition, localhost is the container: suggest host.docker.internal. */
    const hostNote = h("p", { class: "driver-note" });
    const updateHostNote = () => {
      let candidate = host.value;
      if (mode === "string") {
        try {
          candidate = parseConnectionString(driver.id, connectionString.value).host;
        } catch {
          candidate = "";
        }
      }
      hostNote.textContent = localhostHint(candidate);
      hostNote.hidden = hostNote.textContent === "";
    };
    host.addEventListener("input", updateHostNote);
    connectionString.addEventListener("input", updateHostNote);
    const stringField = h("label", { class: "field" }, h("span", { class: "field-label" }, "Connection string"), connectionString, stringHint);
    const paramsSection = h("div", { class: "form-grid" });
    const modeButtons = (["params", "string"] as const).map((value) => {
      const button = h("button", { class: "segment", type: "button", "data-mode": value }, value === "params" ? "Parameters" : "Connection string");
      button.addEventListener("click", () => switchMode(value));
      return button;
    });
    const currentFields = (): ConnectionFields => ({
      host: host.value.trim(),
      port: Number(port.value) || driver.defaultPort,
      user: user.value.trim(),
      database: database.value.trim() || null,
      options: { ...options, ...readOptions() },
    });
    /** Fills the parameter inputs from a parsed string, so either tab shows the same connection. */
    const applyParsed = (parsed: ParsedConnection) => {
      host.value = parsed.host;
      port.value = String(parsed.port);
      user.value = parsed.user;
      database.value = parsed.database ?? "";
      if (parsed.password !== null) password.value = parsed.password;
      Object.assign(options, parsed.options);
      if (driver.id !== "mongodb") delete options.uri;
      renderOptions();
    };
    const renderMode = () => {
      updateHostNote();
      for (const button of modeButtons) button.classList.toggle("active", button.dataset.mode === mode);
      paramsSection.hidden = mode !== "params";
      stringField.hidden = mode !== "string";
      stringHint.textContent = `e.g. ${CONNECTION_STRING_EXAMPLES[driver.id]}. A password in the string goes to ${capabilities().secretStore}, not into the config file.`;
    };
    const switchMode = (next: "params" | "string") => {
      if (next === mode) return;
      if (next === "string") {
        if (driver.id === "mongodb") delete options.uri;
        connectionString.value = formatConnectionString(driver.id, currentFields());
      } else if (connectionString.value.trim()) {
        try {
          applyParsed(parseConnectionString(driver.id, connectionString.value));
        } catch (error) {
          toast(errorText(error), "error");
          return;
        }
      }
      if (next === "params") delete options.uri;
      mode = next;
      renderMode();
    };

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
      availability.textContent = driver.available
        ? ""
        : `${driver.label} support is in progress: the profile can be saved now, connecting comes in a later build.`;
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
      if (mode === "string") {
        delete options.uri;
        connectionString.value = formatConnectionString(next.id, currentFields());
      }
      renderMode();
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
    /** In string mode the string is the source of truth; its password, if any, is returned by readPassword. */
    let stringPassword: string | null = null;
    const readProfile = (): ConnectionProfile => {
      if (mode === "string") {
        const parsed = parseConnectionString(driver.id, connectionString.value);
        stringPassword = parsed.password;
        const label = parsed.options.uri ? parsed.host : `${parsed.host}:${parsed.port}`;
        return {
          id,
          name: name.value.trim() || label,
          driver: driver.id,
          host: parsed.host,
          port: parsed.port,
          user: parsed.user,
          database: parsed.database,
          savePassword: remember.input.checked,
          options: { ...parsed.options, [MODE_OPTION]: "string" },
        };
      }
      stringPassword = null;
      return {
        id,
        name: name.value.trim() || `${host.value.trim()}:${port.value}`,
        driver: driver.id,
        host: host.value.trim(),
        port: Number(port.value) || driver.defaultPort,
        user: user.value.trim(),
        database: database.value.trim() || null,
        savePassword: remember.input.checked,
        options: readOptions(),
      };
    };
    const readPassword = (): string | null => {
      if (mode === "string") return stringPassword ?? (existing?.savePassword ? null : "");
      return password.value === "" && existing?.savePassword ? null : password.value;
    };
    if (mode === "string") connectionString.value = options.uri ?? formatConnectionString(driver.id, currentFields());
    paramsSection.append(
      h("div", { class: "field-row" }, field("Host", host), h("div", { class: "narrow" }, field("Port", port))),
      h("div", { class: "field-row even" }, field("User", user), field("Password", password)),
      databaseField,
      optionsHost,
    );
    renderMode();

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Saved in workspace ", h("strong", {}, workspaceName), "."),
      field("Database type", h("div", { class: "driver-cards" }, ...cards)),
      availability,
      field("Connection name", name),
      h("div", { class: "segmented" }, ...modeButtons),
      paramsSection,
      stringField,
      hostNote,
      remember.element,
    );
    openModal(
      existing ? "Edit connection" : "New connection",
      body,
      [
        {
          label: "Test connection",
          alignLeft: true,
          run: async () => {
            const profile = readProfile();
            const info = await api.testConnection(profile, readPassword()).catch((error: unknown) => {
              // An untrusted (self-signed) SQL Server certificate: point at the option that fixes it.
              const trust = optionInputs.get("trustServerCertificate");
              if (trust && !trust.checked && /trust server certificate/i.test(errorText(error))) {
                trust.closest("label")?.classList.add("attention");
                trust.focus();
              }
              throw error;
            });
            toast(`Connection OK · ${describeServer(profile, info.server.version)}`, "success");
            return false;
          },
        },
        { label: "Cancel", run: () => undefined },
        { label: "Save", primary: true, run: async () => void (saved = await api.saveConnection(readProfile(), readPassword())) },
      ],
      { width: 520, onClose: () => resolve(saved) },
    );
  });
}
