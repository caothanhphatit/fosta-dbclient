import { describe, expect, it } from "vitest";
import { AttemptTimer } from "./attemptTimer";

describe("AttemptTimer", () => {
  it("counts only the last attempt, not the time spent in a confirmation dialog", async () => {
    let clock = 0;
    const timer = new AttemptTimer(() => clock);
    const states: boolean[] = [];
    await expect(
      timer.attempt(
        async () => {
          clock += 5;
          throw new Error("CONFIRMATION_REQUIRED");
        },
        (running) => states.push(running),
      ),
    ).rejects.toThrow();
    clock += 30_000; // the user reads the dialog
    await timer.attempt(
      async () => {
        clock += 8;
      },
      (running) => states.push(running),
    );
    expect(timer.elapsed()).toBe(8);
    expect(states).toEqual([true, false, true, false]);
  });

  it("reports 0 before any attempt", () => {
    expect(new AttemptTimer(() => 42).elapsed()).toBe(0);
  });
});
