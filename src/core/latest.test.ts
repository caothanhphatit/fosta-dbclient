import { describe, expect, it } from "vitest";
import { LatestRequest } from "./latest";

describe("LatestRequest", () => {
  it("drops an answer that arrives after a newer request started", () => {
    const latest = new LatestRequest();
    const slowPage = latest.begin();
    const newerPage = latest.begin();
    expect(latest.isCurrent(slowPage)).toBe(false);
    expect(latest.isCurrent(newerPage)).toBe(true);
  });

  it("drops every answer once the owner closed", () => {
    const latest = new LatestRequest();
    const pending = latest.begin();
    latest.close();
    expect(latest.isCurrent(pending)).toBe(false);
  });

  it("cancels a running request when a newer one starts or the owner closes", () => {
    const latest = new LatestRequest();
    const cancelled: string[] = [];
    const first = latest.begin(() => cancelled.push("first"));
    latest.begin(() => cancelled.push("second"));
    expect(cancelled).toEqual(["first"]);
    latest.finish(first);
    latest.close();
    expect(cancelled).toEqual(["first", "second"]);
  });

  it("does not cancel a request that already finished", () => {
    const latest = new LatestRequest();
    const cancelled: string[] = [];
    const done = latest.begin(() => cancelled.push("done"));
    latest.finish(done);
    latest.begin();
    latest.close();
    expect(cancelled).toEqual([]);
  });
});
