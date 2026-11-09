import { describe, expect, it } from "vitest";
import type { ConnectionSpec, EngineDescriptor, TlsConfig } from "../../core/contract";
import type { ConnectionProfile } from "../../core/types";
import { DESCRIPTORS } from "../../dev/mock/descriptors";
import {
  applySpec,
  authInputs,
  defaultName,
  formFromProfile,
  formLayout,
  formSecrets,
  newForm,
  profileFromForm,
  sameTarget,
  switchEngine,
} from "./connectionForm";
import { sshFromValues, sshValues } from "./sshSection";
import { fallbackMode, tlsFromValues, tlsValues } from "./tlsSection";

const engine = (id: string): EngineDescriptor => DESCRIPTORS[id];

const TLS: TlsConfig = {
  mode: "require",
  caFile: null,
  clientCertFile: null,
  clientKeyFile: null,
  serverName: null,
  acceptInvalidCerts: true,
  pinnedSha256: null,
};

function spec(fields: Partial<ConnectionSpec>): ConnectionSpec {
  return { engine: "", host: "", port: 0, user: "", database: null, file: null, url: null, options: {}, tls: null, ssh: null, auth: null, ...fields };
}

describe("formLayout", () => {
  it("shows host, port and the database for server engines", () => {
    const layout = formLayout(engine("mariadb"));
    expect(layout.host).toBe(true);
    expect(layout.file).toBeNull();
    expect(layout.database?.label).toBe("Default database");
    expect(layout.database?.placeholder).toBe("optional");
    expect(layout.urlTab).toBe(true);
  });

  it("uses the file field and hides host and database for file engines", () => {
    const layout = formLayout(engine("sqlite"));
    expect(layout.host).toBe(false);
    expect(layout.database).toBeNull();
    expect(layout.file?.kind.type).toBe("file");
    expect(layout.general.map((field) => field.key)).not.toContain("file");
  });

  it("hides the database when its label is empty and sorts fields by section", () => {
    const redis = formLayout(engine("redis"));
    expect(redis.database).toBeNull();
    expect(redis.general.map((field) => field.key)).toContain("options.db");
    const es = formLayout(engine("elasticsearch"));
    expect(es.database).toBeNull();
    expect(es.general.map((field) => field.key)).toEqual(["options.cloudId"]);
    expect(es.advanced.map((field) => field.key)).toEqual(["url", "options.requestTimeoutMs"]);
  });
});

describe("authInputs", () => {
  it("asks for user and password, or a password alone where an ACL user method exists", () => {
    expect(authInputs(engine("mariadb"), "password")).toEqual({ user: true, password: true, secret: null });
    expect(authInputs(engine("redis"), "password")).toEqual({ user: false, password: true, secret: null });
    expect(authInputs(engine("redis"), "aclUser")).toEqual({ user: true, password: true, secret: null });
    expect(authInputs(engine("mongodb"), "none")).toEqual({ user: false, password: false, secret: null });
  });

  it("maps API keys and tokens to their secret slots", () => {
    expect(authInputs(engine("elasticsearch"), "apiKey").secret?.slot).toBe("apiKey");
    expect(authInputs(engine("elasticsearch"), "bearer").secret?.slot).toBe("token");
  });
});

describe("form and profile", () => {
  it("starts a new form on the engine defaults", () => {
    const form = newForm(engine("mssql"), "127.0.0.1");
    expect(form).toMatchObject({ engine: "mssql", host: "127.0.0.1", port: "1433", user: "sa", auth: "password", savePassword: true });
    expect(newForm(engine("sqlite"), "127.0.0.1")).toMatchObject({ host: "", port: "", options: { readOnly: "false" } });
  });

  it("round-trips a saved profile with url, tls and auth", () => {
    const profile: ConnectionProfile = {
      id: "c1",
      name: "Events",
      driver: "mongodb",
      host: "cluster0.example.net",
      port: 27017,
      user: "app",
      database: "admin",
      url: "mongodb+srv://app@cluster0.example.net/events?retryWrites=true",
      savePassword: true,
      options: { appName: "kept" },
      auth: "password",
      tls: TLS,
      safety: null,
    };
    const descriptor = engine("mongodb");
    const form = formFromProfile(profile, descriptor);
    const back = profileFromForm("c1", form, descriptor, profile.tls ?? null, null, profile);
    expect(back).toEqual({ ...profile, safety: undefined });
  });

  it("stores only the fields the engine uses", () => {
    const descriptor = engine("sqlite");
    const form = { ...newForm(descriptor, "127.0.0.1"), file: "C:\\data\\shop.db", user: "ignored", database: "ignored" };
    const profile = profileFromForm("s1", form, descriptor, null, null);
    expect(profile).toEqual({
      id: "s1",
      name: "shop.db",
      driver: "sqlite",
      host: "",
      port: 0,
      user: "",
      database: null,
      file: "C:\\data\\shop.db",
      savePassword: true,
      options: { readOnly: "false", create: "false" },
    });
  });

  it("keeps options the form does not show, drops the old tab option", () => {
    const descriptor = engine("mariadb");
    const base: ConnectionProfile = {
      id: "m",
      name: "M",
      driver: "mariadb",
      host: "h",
      port: 3306,
      user: "root",
      database: null,
      savePassword: false,
      options: { charset: "utf8mb4", connectionMode: "string" },
    };
    const profile = profileFromForm("m", formFromProfile(base, descriptor), descriptor, null, null, base);
    expect(profile.options).toEqual({ charset: "utf8mb4" });
    expect(profile.auth).toBeUndefined();
  });

  it("moves port and user with the engine only when they were defaults", () => {
    const mariadb = engine("mariadb");
    const moved = switchEngine(newForm(mariadb, "127.0.0.1"), mariadb, engine("postgres"), "127.0.0.1");
    expect(moved).toMatchObject({ engine: "postgres", port: "5432", user: "postgres" });
    const custom = { ...newForm(mariadb, "127.0.0.1"), port: "13306", user: "app", url: "mariadb://x" };
    expect(switchEngine(custom, mariadb, engine("postgres"), "127.0.0.1")).toMatchObject({ port: "13306", user: "app", url: "" });
  });

  it("names a connection after its host or file", () => {
    const descriptor = engine("mariadb");
    expect(defaultName({ ...newForm(descriptor, "db.example"), port: "3307" }, descriptor)).toBe("db.example:3307");
  });
});

describe("applySpec (URL tab)", () => {
  it("fills the form and puts the password into the password field only", () => {
    const descriptor = engine("mariadb");
    const parsed = spec({ engine: "mariadb", host: "db.example", port: 3307, user: "root", database: "shop", tls: { ...TLS, acceptInvalidCerts: false } });
    const form = applySpec(newForm(descriptor, "127.0.0.1"), parsed, "secret", descriptor);
    expect(form).toMatchObject({ host: "db.example", port: "3307", user: "root", database: "shop", url: "", password: "secret" });
    expect(JSON.stringify(form.options)).not.toContain("secret");
    const profile = profileFromForm("n", form, descriptor, parsed.tls, null);
    expect(JSON.stringify(profile)).not.toContain("secret");
    expect(profile.tls?.mode).toBe("require");
  });

  it("keeps a typed password when the string has none, and the url of SRV strings", () => {
    const descriptor = engine("mongodb");
    const before = { ...newForm(descriptor, "127.0.0.1"), password: "typed" };
    const url = "mongodb+srv://app@cluster0.example.net/events";
    const form = applySpec(before, spec({ host: "cluster0.example.net", port: 27017, user: "app", url }), null, descriptor);
    expect(form).toMatchObject({ password: "typed", url, user: "app", host: "cluster0.example.net" });
  });
});

describe("formSecrets", () => {
  it("keeps the saved password when none is typed, and sends other slots by name", () => {
    const descriptor = engine("elasticsearch");
    const form = { ...newForm(descriptor, "h"), password: "" };
    expect(formSecrets(form, descriptor, true)).toEqual({ password: null, secrets: {} });
    expect(formSecrets(form, descriptor, false).password).toBe("");
    const apiKey = { ...form, auth: "apiKey" as const, secrets: { apiKey: "k-123", token: "unused" } };
    expect(formSecrets(apiKey, descriptor, true, { sshPassword: "s" })).toEqual({ password: "", secrets: { apiKey: "k-123", sshPassword: "s" } });
  });
});

describe("TLS and SSH sections", () => {
  const support = engine("mssql").connection.tls!;

  it("keeps the driver default when nothing is set", () => {
    expect(tlsFromValues(tlsValues(null), support)).toBeNull();
    expect(tlsFromValues(tlsValues(TLS), support)).toEqual(TLS);
  });

  it("encrypts when a setting is made on the driver default", () => {
    expect(fallbackMode(support)).toBe("require");
    expect(tlsFromValues({ ...tlsValues(null), acceptInvalidCerts: true }, support)).toEqual(TLS);
  });

  it("drops settings the engine does not offer", () => {
    const values = { ...tlsValues(TLS), caFile: "ca.pem", pinnedSha256: "ab" };
    expect(tlsFromValues(values, support)).toEqual(TLS);
    expect(tlsFromValues(values, engine("elasticsearch").connection.tls!)).toMatchObject({ caFile: "ca.pem", pinnedSha256: "ab" });
  });

  it("saves SSH only when used, and keeps what the section does not edit", () => {
    expect(sshFromValues(sshValues(null), null)).toBeNull();
    const on = sshFromValues({ ...sshValues(null), enabled: true, host: "bastion", user: "ubuntu", keyFile: "~/.ssh/id_ed25519" }, null);
    expect(on).toEqual({
      enabled: true,
      host: "bastion",
      port: 22,
      user: "ubuntu",
      auth: { method: "privateKey", keyFile: "~/.ssh/id_ed25519" },
      jumpHosts: [],
      hostKey: "acceptNew",
      keepaliveSecs: null,
    });
    const saved = { ...on!, jumpHosts: [{ host: "j", port: 22, user: "u" }], keepaliveSecs: 30 };
    expect(sshFromValues({ ...sshValues(saved), enabled: false }, saved)).toEqual({ ...saved, enabled: false });
  });
});

describe("sameTarget", () => {
  const saved: ConnectionProfile = {
    id: "c",
    name: "Orders",
    driver: "postgres",
    host: "db.internal",
    port: 5432,
    user: "app",
    database: null,
    savePassword: true,
    options: {},
    tls: { ...TLS, mode: "verifyFull", acceptInvalidCerts: false, caFile: "ca.pem" },
    ssh: { enabled: true, host: "bastion", port: 22, user: "me", auth: { method: "password" }, jumpHosts: [], hostKey: "acceptNew", keepaliveSecs: null },
  };

  it("ignores the name, database, keepalive and blanks", () => {
    expect(sameTarget(saved, { ...saved, name: "Renamed", database: "other", ssh: { ...saved.ssh!, keepaliveSecs: 30, host: " bastion " } })).toBe(true);
    const plain = { ...saved, tls: undefined, ssh: undefined };
    expect(sameTarget(plain, { ...plain, ssh: { ...saved.ssh!, enabled: false } })).toBe(true);
  });

  it("notices a new server, tunnel or TLS check", () => {
    const changed: Partial<ConnectionProfile>[] = [
      { host: "evil" },
      { port: 1 },
      { user: "root" },
      { ssh: { ...saved.ssh!, host: "evil" } },
      { ssh: { ...saved.ssh!, enabled: false } },
      { ssh: { ...saved.ssh!, hostKey: "off" } },
      { ssh: { ...saved.ssh!, jumpHosts: [{ host: "j", port: 22, user: "u" }] } },
      { tls: { ...saved.tls!, mode: "disable" } },
      { tls: { ...saved.tls!, acceptInvalidCerts: true } },
      { tls: { ...saved.tls!, pinnedSha256: "ab:cd" } },
      { tls: undefined },
    ];
    for (const change of changed) expect(sameTarget(saved, { ...saved, ...change }), JSON.stringify(change)).toBe(false);
  });
});
