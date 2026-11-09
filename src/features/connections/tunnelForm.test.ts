import { describe, expect, it } from "vitest";
import type { ConnectionProfile } from "../../core/types";
import { sameTarget, withTunnel } from "./connectionForm";
import { endpointForTemplate, isTunnelTaskId, localPort, portForwardTemplate, tunnelFromValues, tunnelTask, tunnelTaskId, tunnelValues } from "./tunnelForm";

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

describe("tunnel command form", () => {
  it("splits arguments like the task dialog and keeps quoted ones together", () => {
    const tunnel = tunnelFromValues({ program: " kubectl ", args: `port-forward -n shop "svc/my db" 13306:3306`, readyPattern: " ", timeout: "45" });
    expect(tunnel).toEqual({ program: "kubectl", args: ["port-forward", "-n", "shop", "svc/my db", "13306:3306"], readyPattern: null, readyTimeoutSecs: 45 });
    expect(tunnelValues(tunnel)).toEqual({ program: "kubectl", args: `port-forward -n shop "svc/my db" 13306:3306`, readyPattern: "", timeout: "45" });
  });

  it("is empty without a program and falls back to the default timeout", () => {
    expect(tunnelFromValues({ program: "  ", args: "port-forward", readyPattern: "", timeout: "30" })).toBeNull();
    expect(tunnelFromValues({ program: "ssh", args: "", readyPattern: "", timeout: "abc" })?.readyTimeoutSecs).toBe(30);
    expect(tunnelValues(null)).toEqual({ program: "", args: "", readyPattern: "", timeout: "30" });
  });

  it("finds the local port of a port-forward", () => {
    expect(localPort(["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"])).toBe(13306);
    expect(localPort(["port-forward", "svc/postgres", "5432"])).toBe(5432);
    expect(localPort(["port-forward", "svc/postgres", ":5432"])).toBeNull();
    expect(localPort(["-L", "99999:db:5432"])).toBeNull();
  });

  it("fills the template for the engine and points an empty or local host at it", () => {
    const mariadb = portForwardTemplate("mariadb", 3306);
    expect(mariadb.program).toBe("kubectl");
    expect(mariadb.args).toBe("port-forward -n default svc/mariadb 13306:3306");
    expect(mariadb.readyPattern).toBe("Forwarding from");
    expect(portForwardTemplate("mongodb", 27017).args).toBe("port-forward -n default svc/mongodb 37017:27017");
    expect(portForwardTemplate("custom", 60000).args).toBe("port-forward -n default svc/custom 60001:60000");
    expect(endpointForTemplate("", mariadb)).toEqual({ host: "127.0.0.1", port: 13306 });
    expect(endpointForTemplate("localhost", mariadb)).toEqual({ host: "127.0.0.1", port: 13306 });
    expect(endpointForTemplate("db.internal", mariadb)).toBeNull();
  });

  it("names the task the backend runs the tunnel under", () => {
    expect(tunnelTaskId("k")).toBe("tunnel:k");
    expect(isTunnelTaskId("tunnel:k")).toBe(true);
    expect(isTunnelTaskId("tunnel-probe:3")).toBe(true);
    expect(isTunnelTaskId("t-a-1")).toBe(false);
    const task = tunnelTask("k", "Shop", { program: "kubectl", args: ["port-forward"], readyTimeoutSecs: 30 });
    expect(task).toMatchObject({ id: "tunnel:k", name: "Tunnel · Shop", readyPattern: null, autoStart: false });
  });

  it("sets or removes the profile's tunnel, and another command is another target", () => {
    const command = { program: "kubectl", args: ["port-forward", "svc/mariadb", "13306:3306"], readyTimeoutSecs: 30 };
    const tunnelled = withTunnel(profile, command);
    expect(tunnelled.tunnel).toEqual(command);
    expect("tunnel" in withTunnel(tunnelled, null)).toBe(false);
    expect(sameTarget(tunnelled, withTunnel(profile, { ...command, readyTimeoutSecs: 90, readyPattern: "Forwarding" }))).toBe(true);
    expect(sameTarget(tunnelled, withTunnel(profile, { ...command, args: ["port-forward", "svc/other", "13306:3306"] }))).toBe(false);
    expect(sameTarget(tunnelled, profile)).toBe(false);
  });
});
