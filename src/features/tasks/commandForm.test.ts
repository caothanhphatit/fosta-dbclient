import { describe, expect, it } from "vitest";
import type { ConnectionProfile } from "../../core/types";
import { sameTarget, withCommand } from "../connections/connectionForm";
import { commandUsers, commandValues, endpointForCommand, localPort, portForwardTemplate, readyTimeout, taskFromValues } from "./commandForm";

const profile: ConnectionProfile = {
  id: "k",
  name: "Shop",
  driver: "mariadb",
  host: "127.0.0.1",
  port: 13306,
  user: "root",
  database: "shop",
  savePassword: true,
  options: {},
};

describe("command form", () => {
  it("splits arguments and keeps quoted ones together", () => {
    const values = { name: " ", program: " kubectl ", args: `port-forward -n shop "svc/my db" 13306:3306`, readyPattern: " ", timeout: "45" };
    const task = taskFromValues("t", values, false, true);
    expect(task).toEqual({
      id: "t",
      name: "kubectl",
      program: "kubectl",
      args: ["port-forward", "-n", "shop", "svc/my db", "13306:3306"],
      autoStart: false,
      autoRestart: true,
      readyPattern: null,
      readyTimeoutSecs: 45,
    });
    expect(commandValues(task)).toEqual({
      name: "kubectl",
      program: "kubectl",
      args: `port-forward -n shop "svc/my db" 13306:3306`,
      readyPattern: "",
      timeout: "45",
    });
  });

  it("needs a program and falls back to the default timeout", () => {
    expect(() => taskFromValues("t", { ...commandValues(null), program: "  " }, true, true)).toThrow("Program is required");
    expect(readyTimeout("abc")).toBe(30);
    expect(readyTimeout("0")).toBe(30);
    expect(readyTimeout("9999")).toBe(600);
    expect(commandValues(null).timeout).toBe("30");
  });

  it("finds the local port of a port-forward", () => {
    expect(localPort(["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"])).toBe(13306);
    expect(localPort(["port-forward", "svc/postgres", "5432"])).toBe(5432);
    expect(localPort(["port-forward", "svc/postgres", ":5432"])).toBeNull();
    expect(localPort(["-L", "99999:db:5432"])).toBeNull();
  });

  it("fills the template for the engine, on the connection's local port when it expects a forward", () => {
    const mariadb = portForwardTemplate({ engineId: "mariadb", engineLabel: "MariaDB", defaultPort: 3306 });
    expect(mariadb).toMatchObject({ name: "MariaDB port-forward", program: "kubectl", readyPattern: "Forwarding from" });
    expect(mariadb.args).toBe("port-forward -n default svc/mariadb 13306:3306");
    expect(portForwardTemplate({ engineId: "mongodb", defaultPort: 27017 }).args).toBe("port-forward -n default svc/mongodb 37017:27017");
    expect(portForwardTemplate({ engineId: "custom", defaultPort: 60000 }).args).toBe("port-forward -n default svc/custom 60001:60000");
    const own = { engineId: "postgres", defaultPort: 5432, host: "127.0.0.1", port: 15433 };
    expect(portForwardTemplate(own).args).toBe("port-forward -n default svc/postgres 15433:5432");
    expect(portForwardTemplate({ ...own, port: 5432 }).args).toBe("port-forward -n default svc/postgres 15432:5432");
    expect(portForwardTemplate({ ...own, host: "db.internal" }).args).toBe("port-forward -n default svc/postgres 15432:5432");
  });

  it("points an empty or local host at the command's local port", () => {
    const task = { args: ["port-forward", "svc/mariadb", "13306:3306"] };
    expect(endpointForCommand("", task)).toEqual({ host: "127.0.0.1", port: 13306 });
    expect(endpointForCommand("localhost", task)).toEqual({ host: "127.0.0.1", port: 13306 });
    expect(endpointForCommand("db.internal", task)).toBeNull();
    expect(endpointForCommand("", { args: ["tail", "-f"] })).toBeNull();
  });

  it("lists who uses a command, and another command is another target", () => {
    const using = withCommand(profile, "pf");
    expect(using.commandId).toBe("pf");
    expect("commandId" in withCommand(using, null)).toBe(false);
    expect(commandUsers("pf", [using, profile, withCommand({ ...profile, id: "j" }, "other")])).toEqual([using]);
    expect(sameTarget(using, { ...using, name: "Renamed" })).toBe(true);
    expect(sameTarget(using, withCommand(profile, "other"))).toBe(false);
    expect(sameTarget(using, profile)).toBe(false);
  });
});
