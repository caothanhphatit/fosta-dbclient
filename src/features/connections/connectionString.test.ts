import { describe, expect, it } from "vitest";
import { formatConnectionString, parseConnectionString, type ConnectionFields } from "./connectionString";

describe("parseConnectionString: MariaDB / MySQL", () => {
  it("reads user, password, host, port and database from a URL", () => {
    expect(parseConnectionString("mariadb", "mysql://root:p%40ss@db.local:3307/shop")).toEqual({
      host: "db.local",
      port: 3307,
      user: "root",
      password: "p@ss",
      database: "shop",
      options: {},
    });
  });

  it("accepts jdbc: and mariadb:// prefixes and a database query parameter", () => {
    const parsed = parseConnectionString("mysql", "jdbc:mariadb://10.0.0.5/?database=crm");
    expect(parsed).toMatchObject({ host: "10.0.0.5", port: 3306, user: "", password: null, database: "crm" });
  });

  it("keeps the password null when the string has none, so the saved one is kept", () => {
    expect(parseConnectionString("mariadb", "mysql://app@127.0.0.1").password).toBeNull();
  });

  it("uses the first host of a host list and reads bracketed IPv6", () => {
    expect(parseConnectionString("mariadb", "mysql://u@a:3310,b:3311/x")).toMatchObject({ host: "a", port: 3310 });
    expect(parseConnectionString("mariadb", "mysql://u@[::1]:3308/x")).toMatchObject({ host: "::1", port: 3308 });
  });

  it("rejects other schemes and empty input", () => {
    expect(() => parseConnectionString("mariadb", "postgres://u@h/db")).toThrow("Expected mysql://");
    expect(() => parseConnectionString("mariadb", "   ")).toThrow("Paste a connection string first");
  });
});

describe("parseConnectionString: SQL Server", () => {
  it("parses ADO.NET keys case-insensitively, including tcp: and a port after a comma", () => {
    const parsed = parseConnectionString(
      "mssql",
      "Server=tcp:sql.example.com,1444;Initial Catalog=ERP;User ID=sa;Password=secret;Encrypt=False;TrustServerCertificate=yes",
    );
    expect(parsed).toEqual({
      host: "sql.example.com",
      port: 1444,
      user: "sa",
      password: "secret",
      database: "ERP",
      options: { encrypt: "false", trustServerCertificate: "true" },
    });
  });

  it("reads {braced} values that contain ; and escaped }} braces", () => {
    const parsed = parseConnectionString("mssql", "Data Source=db;Database={my;db};Pwd={p;a}}ss};User Id=app");
    expect(parsed).toMatchObject({ host: "db", database: "my;db", password: "p;a}ss", user: "app" });
  });

  it("parses the JDBC form", () => {
    const parsed = parseConnectionString("mssql", "jdbc:sqlserver://sql01:1435;databaseName=Sales;user=reporter;password=pw;encrypt=true");
    expect(parsed).toMatchObject({ host: "sql01", port: 1435, user: "reporter", password: "pw", database: "Sales" });
    expect(parsed.options).toEqual({ encrypt: "true", trustServerCertificate: "false" });
  });

  it("drops a named instance, honours Port= and defaults to TLS on", () => {
    const parsed = parseConnectionString("mssql", String.raw`Server=db\SQLEXPRESS;Port=1500`);
    expect(parsed).toMatchObject({ host: "db", port: 1500, user: "", password: null, database: null });
    expect(parsed.options.encrypt).toBe("true");
  });

  it("requires a server", () => {
    expect(() => parseConnectionString("mssql", "Database=ERP;User Id=sa")).toThrow("Expected Server=");
  });
});

describe("parseConnectionString: MongoDB", () => {
  it("keeps the URI without its password and reads authSource as the database", () => {
    const parsed = parseConnectionString("mongodb", "mongodb+srv://user:secret@cluster0.example.net/?authSource=admin&retryWrites=true");
    expect(parsed).toEqual({
      host: "cluster0.example.net",
      port: 27017,
      user: "user",
      password: "secret",
      database: "admin",
      options: { uri: "mongodb+srv://user@cluster0.example.net/?authSource=admin&retryWrites=true" },
    });
  });

  it("uses the first host of a replica set and the path as the database", () => {
    const parsed = parseConnectionString("mongodb", "mongodb://a:27018,b:27019/events?replicaSet=rs0");
    expect(parsed).toMatchObject({ host: "a", port: 27018, user: "", password: null, database: "events" });
    expect(parsed.options.uri).toBe("mongodb://a:27018,b:27019/events?replicaSet=rs0");
  });

  it("rejects other schemes", () => {
    expect(() => parseConnectionString("mongodb", "mysql://u@h/db")).toThrow("Expected mongodb://");
  });
});

describe("formatConnectionString", () => {
  const fields = (overrides: Partial<ConnectionFields>): ConnectionFields => ({
    host: "127.0.0.1",
    port: 3306,
    user: "root",
    database: "shop",
    options: {},
    ...overrides,
  });

  it("writes a MySQL URL without a password, encoding the user", () => {
    expect(formatConnectionString("mariadb", fields({}))).toBe("mysql://root@127.0.0.1:3306/shop");
    expect(formatConnectionString("mysql", fields({ user: "a@b", database: null }))).toBe("mysql://a%40b@127.0.0.1:3306");
  });

  it("writes SQL Server key=value pairs with the TLS flags", () => {
    const tls = { encrypt: "false", trustServerCertificate: "true" };
    expect(formatConnectionString("mssql", fields({ port: 1433, user: "sa", database: "ERP", options: tls }))).toBe(
      "Server=127.0.0.1,1433;Database=ERP;User Id=sa;Encrypt=False;TrustServerCertificate=True",
    );
    expect(formatConnectionString("mssql", fields({ port: 1433, user: "", database: null }))).toBe("Server=127.0.0.1,1433;Encrypt=True");
  });

  it("returns a saved MongoDB URI as-is, otherwise builds one", () => {
    expect(formatConnectionString("mongodb", fields({ options: { uri: "mongodb+srv://u@x.net/" } }))).toBe("mongodb+srv://u@x.net/");
    expect(formatConnectionString("mongodb", fields({ port: 27017, database: "admin" }))).toBe("mongodb://root@127.0.0.1:27017/?authSource=admin");
    expect(formatConnectionString("mongodb", fields({ port: 27017, user: "", database: null }))).toBe("mongodb://127.0.0.1:27017/");
  });

  it("round-trips through parse for every driver", () => {
    const cases: [Parameters<typeof formatConnectionString>[0], ConnectionFields][] = [
      ["mariadb", fields({})],
      ["mysql", fields({ host: "db", port: 3310, database: null })],
      ["mssql", fields({ port: 1433, user: "sa", database: "ERP", options: { encrypt: "true", trustServerCertificate: "false" } })],
    ];
    for (const [driver, input] of cases) {
      const parsed = parseConnectionString(driver, formatConnectionString(driver, input));
      expect(parsed).toMatchObject({ host: input.host, port: input.port, user: input.user, database: input.database, password: null });
    }
  });
});
