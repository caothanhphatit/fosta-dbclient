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
});
