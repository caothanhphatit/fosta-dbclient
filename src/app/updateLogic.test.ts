import { describe, expect, it } from "vitest";
import { dockerUpdateCommands, downloadPercent, releaseUrl, updateLabel } from "./updateLogic";

describe("update item", () => {
  it("labels each phase", () => {
    expect(updateLabel("none", "0.1.1")).toBe("");
    expect(updateLabel("available", "0.1.1")).toBe("Update 0.1.1");
    expect(updateLabel("downloading", "0.1.1")).toBe("Update 0.1.1…");
    expect(updateLabel("downloading", "0.1.1", 42)).toBe("Update 0.1.1 · 42%");
    expect(updateLabel("ready", "0.1.1")).toBe("Restart to update");
  });

  it("computes download progress", () => {
    expect(downloadPercent(0, undefined)).toBeNull();
    expect(downloadPercent(10, 0)).toBeNull();
    expect(downloadPercent(0, 200)).toBe(0);
    expect(downloadPercent(199, 200)).toBe(99);
    expect(downloadPercent(250, 200)).toBe(100);
  });

  it("links to the release and keeps the data volume when updating the container", () => {
    expect(releaseUrl("0.1.1")).toBe("https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.1");
    const commands = dockerUpdateCommands();
    expect(commands[0]).toBe("docker pull ghcr.io/caothanhphatit/fosta-studio");
    expect(commands.at(-1)).toContain("-v fosta-studio-data:/data");
  });
});
