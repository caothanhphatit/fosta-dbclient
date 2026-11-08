/**
 * The connection dialog, rendered from the engine's descriptor (`EngineDescriptor.connection`, see `connectionForm.ts`
 * for the mapping): an engine picker (planned engines only with the developer setting), host/port/user/database/file
 * and the extra fields per section, auth methods with their secrets, a Connection URL tab parsed by the backend
 * (`parse_connection_string`), and the TLS and SSH sections (`tlsSection.ts`, `sshSection.ts`). Shared UI: no engine
 * ids here.
 *
 * Secrets never enter the profile: the password goes to `save_connection`'s `password`, other slots to `secrets`.
 * Test connection reports the server (`ConnectInfo.server.label`); when the backend asks for the password
 * (`PASSWORD_REQUIRED`: the target changed, so the saved one is not used), the dialog asks for it and tries again.
 */
import { api, errorCode, errorText, PASSWORD_REQUIRED } from "../../core/api";
import { capabilities, localhostHint } from "../../core/capabilities";
import type { AppError, AuthMethodId, ConnectionSpec, EngineDescriptor, FieldSpec } from "../../core/contract";
import { checkbox, field, openModal, promptPassword, textInput, toast } from "../../core/dialogs";
import { h, icon, iconOrDefault } from "../../core/dom";
import { describeServer, engineInfo, visibleEngines, type EngineInfo } from "../../core/engines";
import { renderError } from "../../core/errorPanel";
import type { AppConfig, ConnectionProfile } from "../../core/types";
import {
  applySpec,
  AUTH_LABELS,
  authInputs,
  fieldTarget,
  formFromProfile,
  formLayout,
  formSecrets,
  newForm,
  profileFromForm,
  switchEngine,
  type ConnectionForm,
} from "./connectionForm";
import "./connectionDialog.css";
import { renderSshSection, type SshSection } from "./sshSection";
import { renderTlsSection, type TlsSection } from "./tlsSection";

/** SQL Server's untrusted (self-signed) certificate error names the TLS option that fixes it. */
const UNTRUSTED_CERTIFICATE = /trust server certificate|accept invalid certificates/i;

function invalid(message: string): AppError {
  return { code: "INVALID_INPUT", message };
}

export function connectionDialog(existing: ConnectionProfile | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const defaultHost = capabilities().inContainer ? "host.docker.internal" : "127.0.0.1";
    // A new connection starts on the first engine offered; a saved one keeps its own (an unknown id stays marked unknown).
    let descriptor: EngineInfo = existing ? engineInfo(existing.driver) : (visibleEngines()[0] ?? engineInfo(undefined));
    let form: ConnectionForm = existing ? formFromProfile(existing, descriptor) : newForm(descriptor, defaultHost);
    let tls: TlsSection = renderTlsSection(existing, descriptor);
    let ssh: SshSection = renderSshSection(existing, descriptor);
    // A profile whose string carries the connection (replica sets, SRV) opens on the URL tab.
    let mode: "params" | "url" = existing?.url && formLayout(descriptor).urlTab && !hasUrlField(descriptor) ? "url" : "params";

    const name = textInput(form.name, { placeholder: "e.g. Staging replica" });
    name.addEventListener("input", () => (form.name = name.value));
    const remember = checkbox(`Save password in ${capabilities().secretStore}`, form.savePassword);
    remember.input.addEventListener("change", () => (form.savePassword = remember.input.checked));
    const availability = h("p", { class: "driver-note" });
    const generalHost = h("div", { class: "form-grid" });
    const authHost = h("div", { class: "form-grid" });
    const advancedBody = h("div", { class: "form-grid conn-section-body" });
    const advanced = h("details", { class: "conn-section" }, h("summary", {}, "Advanced"), advancedBody);
    const sectionsHost = h("div", { class: "conn-sections" });
    const hostNote = h("p", { class: "driver-note" });
    const errorHost = h("div", { class: "conn-feedback" });
    const urlText = h("textarea", { class: "input mono connection-string", rows: "3", spellcheck: "false" });
    urlText.value = mode === "url" ? form.url : "";
    const urlHint = h("span", { class: "field-hint" });
    const urlError = h("div", { class: "conn-feedback" });
    const urlField = h("label", { class: "field" }, h("span", { class: "field-label" }, "Connection URL"), urlText, urlHint);
    const urlSection = h("div", { class: "form-grid" }, urlField, urlError);
    let passwordInput: HTMLInputElement | null = null;

    const modeButtons = (["params", "url"] as const).map((value) => {
      const button = h("button", { class: "segment", type: "button", "data-mode": value }, value === "params" ? "Parameters" : "Connection URL");
      button.addEventListener("click", () => void switchMode(value));
      return button;
    });
    const modeSwitch = h("div", { class: "segmented" }, ...modeButtons);

    const updateHostNote = () => {
      hostNote.textContent = formLayout(descriptor).host ? localhostHint(form.host) : "";
      hostNote.hidden = hostNote.textContent === "";
    };
    const showFeedback = (host: HTMLElement, error: unknown, context: string) => host.replaceChildren(renderError(error, { context }));
    const clearFeedback = () => {
      errorHost.replaceChildren();
      urlError.replaceChildren();
    };

    /** An input for a descriptor field, bound to its place in the form. */
    const fieldControl = (spec: FieldSpec): HTMLElement => {
      const target = fieldTarget(spec);
      const get = (): string => {
        switch (target.kind) {
          case "database":
            return form.database;
          case "file":
            return form.file;
          case "url":
            return form.url;
          case "option":
            return form.options[target.name] ?? "";
          case "secret":
            return target.slot === "password" ? form.password : (form.secrets[target.slot] ?? "");
          default:
            return "";
        }
      };
      const set = (value: string) => {
        switch (target.kind) {
          case "database":
            form.database = value;
            break;
          case "file":
            form.file = value;
            break;
          case "url":
            form.url = value;
            break;
          case "option":
            form.options[target.name] = value;
            break;
          case "secret":
            if (target.slot === "password") form.password = value;
            else form.secrets[target.slot] = value;
            break;
        }
      };
      const label = spec.required ? `${spec.label} *` : spec.label;
      const kind = spec.kind;
      if (kind.type === "checkbox") {
        const box = checkbox(label, get() === "true");
        box.input.addEventListener("change", () => set(String(box.input.checked)));
        return h("div", { class: "field" }, box.element, spec.hint ? h("span", { class: "field-hint" }, spec.hint) : null);
      }
      let control: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;
      if (kind.type === "select") {
        control = h("select", { class: "input" }, ...kind.options.map(([value, text]) => h("option", { value }, text)));
        control.value = get() || (spec.default ?? kind.options[0]?.[0] ?? "");
        set(control.value);
      } else if (kind.type === "textarea") {
        control = h("textarea", { class: "input mono connection-string", rows: "2", spellcheck: "false", placeholder: spec.placeholder ?? "" });
        control.value = get();
      } else if (kind.type === "password") {
        const keep = existing?.savePassword ? "Saved, leave empty to keep" : "";
        control = textInput(get(), { type: "password", placeholder: spec.placeholder ?? keep });
      } else if (kind.type === "file") {
        // No native file picker yet (desktop and web): the path is typed. Filters show what the engine expects.
        const filters = kind.filters.length ? kind.filters.map((filter) => `*.${filter}`).join(", ") : "";
        control = textInput(get(), { class: "input mono", placeholder: spec.placeholder ?? (filters || "/path/to/file") });
      } else {
        control = textInput(get(), { type: kind.type === "number" ? "number" : "text", placeholder: spec.placeholder ?? "" });
      }
      control.addEventListener(kind.type === "select" ? "change" : "input", () => set(control.value));
      return field(label, control, spec.hint ?? "");
    };

    /** The note shown on the Parameters tab when a saved connection string (replica set, SRV) does the connecting. */
    const urlNote = (): HTMLElement | null => {
      if (!form.url || hasUrlField(descriptor)) return null;
      const clear = h("button", { class: "link-button", type: "button" }, "Use these parameters instead");
      const note = h(
        "p",
        { class: "driver-note" },
        "Connects with the connection string of the Connection URL tab (hosts, replica set, options); host and port below only label it. ",
        clear,
      );
      clear.addEventListener("click", () => {
        form.url = "";
        urlText.value = "";
        note.remove();
      });
      return note;
    };

    const renderGeneral = () => {
      const layout = formLayout(descriptor);
      const children: (HTMLElement | null)[] = [urlNote()];
      if (layout.host) {
        const host = textInput(form.host);
        host.addEventListener("input", () => {
          form.host = host.value;
          updateHostNote();
        });
        const port = textInput(form.port, { type: "number", min: 1, max: 65535 });
        port.addEventListener("input", () => (form.port = port.value));
        children.push(h("div", { class: "field-row" }, field("Host", host), h("div", { class: "narrow" }, field("Port", port))));
      }
      if (layout.file) children.push(fieldControl(layout.file));
      if (layout.database) children.push(fieldControl(layout.database));
      children.push(...layout.general.map(fieldControl));
      generalHost.replaceChildren(...children.filter((child): child is HTMLElement => child !== null));
      generalHost.hidden = generalHost.childElementCount === 0;
    };

    const renderAuth = () => {
      const layout = formLayout(descriptor);
      const offered = descriptor.connection.auth;
      const inputs = authInputs(descriptor, form.auth);
      const children: HTMLElement[] = [];
      if (offered.length > 1) {
        const method = h("select", { class: "input" }, ...offered.map((value) => h("option", { value }, AUTH_LABELS[value] ?? value)));
        method.value = form.auth;
        method.addEventListener("change", () => {
          form.auth = method.value as AuthMethodId;
          renderAuth();
        });
        children.push(field("Authentication", method));
      }
      const user = textInput(form.user);
      user.addEventListener("input", () => (form.user = user.value));
      const keep = existing?.savePassword ? "Saved, leave empty to keep" : "";
      const placeholder = mode === "url" ? (keep ? `${keep} (or put it in the URL)` : "If not in the URL") : keep;
      const password = textInput(form.password, { type: "password", placeholder });
      password.addEventListener("input", () => (form.password = password.value));
      passwordInput = inputs.password ? password : null;
      // On the URL tab the string holds the user.
      const userField = inputs.user && mode === "params" ? field("User", user) : null;
      const passwordField = inputs.password ? field("Password", password) : null;
      if (userField && passwordField) children.push(h("div", { class: "field-row even" }, userField, passwordField));
      else if (userField ?? passwordField) children.push((userField ?? passwordField)!);
      if (inputs.secret) {
        const slot = inputs.secret.slot;
        const secret = textInput(form.secrets[slot] ?? "", { type: "password", class: "input mono", placeholder: keep });
        secret.addEventListener("input", () => (form.secrets[slot] = secret.value));
        children.push(field(inputs.secret.label, secret));
      }
      children.push(...layout.auth.map(fieldControl));
      const usesSecrets = inputs.password || inputs.secret !== null || layout.auth.some((spec) => spec.kind.type === "password");
      remember.element.hidden = !usesSecrets;
      remember.element.querySelector("span")!.textContent = `Save ${inputs.password ? "password" : "secrets"} in ${capabilities().secretStore}`;
      children.push(remember.element);
      authHost.replaceChildren(...children);
    };

    const renderAdvanced = () => {
      const fields = formLayout(descriptor).advanced;
      advancedBody.replaceChildren(...fields.map(fieldControl));
      advanced.hidden = fields.length === 0;
    };

    const renderSections = () => sectionsHost.replaceChildren(advanced, tls.element, ssh.element);

    const renderMode = () => {
      const layout = formLayout(descriptor);
      if (!layout.urlTab) mode = "params";
      modeSwitch.hidden = !layout.urlTab;
      for (const button of modeButtons) button.classList.toggle("active", button.dataset.mode === mode);
      generalHost.hidden = mode !== "params" || generalHost.childElementCount === 0;
      urlSection.hidden = mode !== "url";
      const example = descriptor.connection.url?.example ?? "";
      urlText.placeholder = example;
      urlHint.textContent = `e.g. ${example}. A password in the URL goes to ${capabilities().secretStore}, not into the config file.`;
    };

    const renderAll = () => {
      for (const card of cards) card.classList.toggle("active", card.dataset.engine === descriptor.id);
      const planned = descriptor.status === "planned";
      availability.textContent = descriptor.unknown
        ? `This build does not know the engine "${descriptor.id}": the profile is kept as it is, connecting fails with UNKNOWN_ENGINE.`
        : planned
          ? `${descriptor.label} support is planned: the profile can be saved now, connecting comes in a later build (ENGINE_NOT_WIRED).`
          : "";
      availability.hidden = availability.textContent === "";
      renderGeneral();
      renderAuth();
      renderAdvanced();
      renderSections();
      renderMode();
      updateHostNote();
    };

    /** Parses the URL tab's text and fills the form (and the TLS / SSH sections) from it. */
    const applyUrl = async (): Promise<boolean> => {
      urlError.replaceChildren();
      const text = urlText.value.trim();
      if (!text) {
        showFeedback(urlError, invalid(`Paste a connection URL first, e.g. ${descriptor.connection.url?.example ?? ""}`), "Read the connection URL");
        return false;
      }
      let parsed: { spec: ConnectionSpec; password: string | null };
      try {
        parsed = await api.parseConnectionString(descriptor.id, text);
      } catch (error) {
        showFeedback(urlError, error, "Read the connection URL");
        return false;
      }
      form = applySpec(form, parsed.spec, parsed.password, descriptor);
      const sectionProfile = { tls: parsed.spec.tls ?? tls.read() ?? undefined, ssh: parsed.spec.ssh ?? ssh.read() ?? undefined };
      if (parsed.spec.tls) tls = renderTlsSection(sectionProfile, descriptor);
      if (parsed.spec.ssh) ssh = renderSshSection(sectionProfile, descriptor);
      return true;
    };

    const switchMode = async (next: "params" | "url") => {
      if (next === mode) return;
      clearFeedback();
      if (next === "params" && urlText.value.trim() && !(await applyUrl())) return;
      if (next === "url" && !urlText.value.trim() && form.url) urlText.value = form.url;
      mode = next;
      renderAll();
    };

    const selectEngine = (next: EngineInfo) => {
      if (next.id === descriptor.id) return;
      const carried = { tls: tls.read() ?? undefined, ssh: ssh.read() ?? undefined };
      form = switchEngine(form, descriptor, next, defaultHost);
      descriptor = next;
      tls = renderTlsSection(carried, descriptor);
      ssh = renderSshSection(carried, descriptor);
      clearFeedback();
      renderAll();
    };

    // The engines offered, plus the profile's own when it is planned (setting off) or unknown.
    const offered: EngineInfo[] = visibleEngines();
    if (!offered.some((engine) => engine.id === descriptor.id)) offered.push(descriptor);
    const cards = offered.map((engine) => {
      const card = h(
        "button",
        { class: "driver-card", type: "button", "data-engine": engine.id, title: engine.label },
        icon(iconOrDefault(engine.icon, "database"), `driver-icon ${engine.id}`),
        h("span", { class: "driver-label" }, engine.label),
        engine.unknown ? h("span", { class: "driver-soon" }, "unknown") : engine.status === "planned" ? h("span", { class: "driver-soon" }, "planned") : null,
      );
      card.addEventListener("click", () => selectEngine(engine));
      return card;
    });

    /** The profile and secrets to test or save; on the URL tab the string is parsed first. Null after showing why not. */
    const readDraft = async (context: string) => {
      clearFeedback();
      if (mode === "url" && !(await applyUrl())) return null;
      if (mode === "url") renderAll();
      const layout = formLayout(descriptor);
      const missing = [
        layout.host && !form.host.trim() && !form.url.trim() ? "Host" : null,
        layout.file && !form.file.trim() ? layout.file.label : null,
        ...[...layout.general, ...layout.auth, ...layout.advanced]
          .filter((spec) => spec.required && spec.kind.type !== "password" && spec.kind.type !== "checkbox")
          .filter((spec) => {
            const target = fieldTarget(spec);
            if (target.kind === "option") return !(form.options[target.name] ?? "").trim();
            if (target.kind === "url") return !form.url.trim();
            return false;
          })
          .map((spec) => spec.label),
      ].filter((label): label is string => label !== null);
      if (missing.length) {
        showFeedback(errorHost, invalid(`Fill in: ${missing.join(", ")}.`), context);
        return null;
      }
      const profile = profileFromForm(id, form, descriptor, tls.read(), ssh.read(), existing);
      const secrets = formSecrets(form, descriptor, Boolean(existing?.savePassword), { ...tls.secrets(), ...ssh.secrets() });
      return { profile, ...secrets };
    };

    const testConnection = async (): Promise<false> => {
      const draft = await readDraft("Test connection");
      if (!draft) return false;
      let password = draft.password;
      for (;;) {
        try {
          const info = await api.testConnection(draft.profile, password, Object.keys(draft.secrets).length ? draft.secrets : null);
          const ok = h("p", { class: "conn-ok" }, icon("check"), `Connection OK · ${describeServer(info)}`);
          errorHost.replaceChildren(ok);
          toast(`Connection OK · ${describeServer(info)}`, "success");
          return false;
        } catch (error) {
          if (errorCode(error) === PASSWORD_REQUIRED && password === null) {
            // The server changed since the password was saved: the saved one is not sent to another server.
            const answer = await promptPassword(draft.profile.name);
            if (!answer) return false;
            password = answer.password;
            form.password = answer.password;
            form.savePassword = answer.remember;
            remember.input.checked = answer.remember;
            if (passwordInput) passwordInput.value = answer.password;
            continue;
          }
          if (UNTRUSTED_CERTIFICATE.test(errorText(error))) tls.pointAtAcceptInvalid();
          showFeedback(errorHost, error, "Test connection");
          return false;
        }
      }
    };

    const save = async (): Promise<boolean | void> => {
      const draft = await readDraft("Save connection");
      if (!draft) return false;
      try {
        const secrets = Object.keys(draft.secrets).length ? draft.secrets : null;
        saved = await api.saveConnection(draft.profile, draft.password, null, secrets);
      } catch (error) {
        showFeedback(errorHost, error, "Save connection");
        return false;
      }
    };

    renderAll();
    const body = h(
      "div",
      { class: "form-grid connection-dialog" },
      field("Database type", h("div", { class: "driver-cards" }, ...cards)),
      availability,
      field("Connection name", name),
      modeSwitch,
      generalHost,
      urlSection,
      authHost,
      hostNote,
      sectionsHost,
      errorHost,
    );
    openModal(
      existing ? "Edit connection" : "New connection",
      body,
      [
        { label: "Test connection", alignLeft: true, run: testConnection },
        { label: "Cancel", run: () => undefined },
        { label: "Save", primary: true, run: save },
      ],
      { width: 560, onClose: () => resolve(saved) },
    );
  });
}

/** Whether the engine edits `url` as a field of its own (a node list) rather than through the URL tab. */
function hasUrlField(descriptor: EngineDescriptor): boolean {
  return descriptor.connection.fields.some((spec) => spec.key === "url");
}
