import { describe, expect, it } from "vitest";
import { RELAX_CONFIRMATION_REQUIRED, relaxProblem, withRelaxConfirmation, type RelaxProblem } from "./relaxConfirmation";

const refusal = (expected: string) => ({
  code: RELAX_CONFIRMATION_REQUIRED,
  message: `type "${expected}"`,
  expected,
  changes: ["Write statements: blocked → allowed"],
});

describe("withRelaxConfirmation", () => {
  it("saves without a confirmation when none is needed", async () => {
    const calls: (string | null)[] = [];
    const result = await withRelaxConfirmation(
      async (typed) => {
        calls.push(typed);
        return "saved";
      },
      async () => {
        throw new Error("not asked");
      },
    );
    expect(result).toBe("saved");
    expect(calls).toEqual([null]);
  });

  it("asks for the name and retries with it until the backend accepts", async () => {
    const calls: (string | null)[] = [];
    const asked: RelaxProblem[] = [];
    const answers = ["prod", "Prod"];
    const result = await withRelaxConfirmation(
      async (typed) => {
        calls.push(typed);
        if (typed !== "Prod") throw refusal("Prod");
        return "saved";
      },
      async (problem) => {
        asked.push(problem);
        return answers.shift() ?? null;
      },
    );
    expect(result).toBe("saved");
    expect(calls).toEqual([null, "prod", "Prod"]);
    expect(asked[0]).toEqual({ expected: "Prod", changes: ["Write statements: blocked → allowed"], message: 'type "Prod"' });
  });

  it("returns null when the prompt is cancelled and rethrows other errors", async () => {
    const cancelled = await withRelaxConfirmation(
      async () => {
        throw refusal("Prod");
      },
      async () => null,
    );
    expect(cancelled).toBeNull();
    await expect(
      withRelaxConfirmation(
        async () => {
          throw { code: "ERROR", message: "boom" };
        },
        async () => "x",
      ),
    ).rejects.toEqual({ code: "ERROR", message: "boom" });
  });

  it("reads only relax refusals", () => {
    expect(relaxProblem({ code: "PASSWORD_REQUIRED", message: "x" })).toBeNull();
    expect(relaxProblem(new Error("x"))).toBeNull();
    expect(relaxProblem({ code: RELAX_CONFIRMATION_REQUIRED, message: "m" })).toEqual({ expected: "", changes: [], message: "m" });
  });
});
