import { describe, expect, it } from "vitest";
import type { ColumnInfo, FilterOp, Value, ValueKind } from "../../core/contract";
import {
  FILTER_OPERATORS,
  columnCondition,
  conditionsFilter,
  filterConditions,
  formatFilter,
  matchesFilter,
  parseFilterText,
  withOperator,
} from "./columnFilter";

const col = (name: string, kind: ValueKind["kind"]): ColumnInfo => ({
  name,
  typeName: kind,
  kind: { kind } as ValueKind,
  nullable: true,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints: {},
});

const name = col("name", "text");
const price = col("price", "decimal");
const created = col("created", "date");
const active = col("active", "bool");

describe("parseFilterText", () => {
  const cases: [string, FilterOp, string[]][] = [
    ["abc", "contains", ["abc"]],
    ["!abc", "notContains", ["abc"]],
    ["^Oak", "startsWith", ["Oak"]],
    ["desk$", "endsWith", ["desk"]],
    ["=x", "eq", ["x"]],
    ["!= x", "ne", ["x"]],
    ["<>x", "ne", ["x"]],
    ["<5", "lt", ["5"]],
    ["<=5", "le", ["5"]],
    [">5", "gt", ["5"]],
    [">= 5", "ge", ["5"]],
    ["1.5..2.5", "between", ["1.5", "2.5"]],
    ["in: a, b ,c", "in", ["a", "b", "c"]],
    ["!in:a,b", "notIn", ["a", "b"]],
    ["~a%b_", "like", ["a%b_"]],
    ["!~a%", "notLike", ["a%"]],
    ["NULL", "isNull", []],
    ["!null", "isNotNull", []],
    ["not null", "isNotNull", []],
  ];
  it.each(cases)("%s → %s", (text, op, values) => {
    expect(parseFilterText(text)).toEqual({ op, values });
  });

  it("covers every operator the menu offers", () => {
    const seen = new Set(cases.map(([, op]) => op));
    expect(FILTER_OPERATORS.filter((item) => !seen.has(item.op))).toEqual([]);
    expect(FILTER_OPERATORS).toHaveLength(17);
  });

  it("reads plain text as equals on non-text columns", () => {
    expect(parseFilterText("42", price.kind)).toEqual({ op: "eq", values: ["42"] });
    expect(parseFilterText("42", name.kind)).toEqual({ op: "contains", values: ["42"] });
  });

  it("keeps a leading = of the value after the operator", () => {
    expect(parseFilterText("==a")).toEqual({ op: "eq", values: ["=a"] });
  });

  it("ignores empty boxes", () => {
    expect(parseFilterText("   ")).toBeNull();
  });
});

describe("formatFilter / withOperator", () => {
  it("round-trips every operator", () => {
    const values: Record<string, string[]> = { between: ["1", "9"], in: ["a", "b"], notIn: ["a", "b"], isNull: [], isNotNull: [] };
    for (const { op } of FILTER_OPERATORS) {
      const text = formatFilter(op, values[op] ?? ["v"]);
      expect(parseFilterText(text), text).toEqual({ op, values: values[op] ?? ["v"] });
    }
  });

  it("switches the operator but keeps the value", () => {
    expect(withOperator(">= 10", "lt")).toBe("<10");
    expect(withOperator("abc", "startsWith")).toBe("^abc");
    expect(withOperator("abc", "isNull")).toBe("null");
    expect(withOperator("null", "eq")).toBe("=");
    expect(withOperator("5", "between")).toBe("5..");
  });
});

describe("columnCondition", () => {
  it("emits structured conditions with the values as typed text", () => {
    expect(columnCondition(name, "O'Brien")).toEqual({ column: "name", op: "contains", values: ["O'Brien"] });
    expect(columnCondition(price, ">= 10.5")).toEqual({ column: "price", op: "ge", values: ["10.5"] });
    expect(columnCondition(created, "2026-01-01..2026-02-01")).toEqual({ column: "created", op: "between", values: ["2026-01-01", "2026-02-01"] });
  });

  it("rejects values that do not fit the column's type", () => {
    expect(columnCondition(price, ">abc")).toBeInstanceOf(Error);
    expect(columnCondition(created, "=2026-13-01")).toBeInstanceOf(Error);
    expect(columnCondition(active, "maybe")).toBeInstanceOf(Error);
    expect(columnCondition(price, "in:")).toBeInstanceOf(Error);
  });

  it("lets pattern operators take any text, even on typed columns", () => {
    expect(columnCondition(price, "~1%")).toEqual({ column: "price", op: "like", values: ["1%"] });
  });
});

describe("filterConditions", () => {
  it("returns the valid conditions and the errors of the others", () => {
    const { conditions, errors } = filterConditions([name, price, created], { name: "desk", price: "> x", created: "" });
    expect(conditions).toEqual([{ column: "name", op: "contains", values: ["desk"] }]);
    expect([...errors.keys()]).toEqual(["price"]);
    expect(conditionsFilter(conditions)).toEqual({ kind: "conditions", conditions, matchAny: false });
    expect(conditionsFilter([])).toEqual({ kind: "none" });
  });
});

describe("matchesFilter", () => {
  const text = (v: string): Value => ({ t: "text", v });
  const dec = (v: string): Value => ({ t: "decimal", v });
  const NULL: Value = { t: "null", v: null };

  it("matches text case-insensitively", () => {
    expect(matchesFilter(text("Hello World"), name, "world")).toBe(true);
    expect(matchesFilter(text("Hello"), name, "^he")).toBe(true);
    expect(matchesFilter(text("Hello"), name, "!ell")).toBe(false);
    expect(matchesFilter(text("Hello"), name, "~H_llo")).toBe(true);
  });

  it("compares numbers numerically", () => {
    expect(matchesFilter(dec("9.5"), price, ">10")).toBe(false);
    expect(matchesFilter(dec("12"), price, ">10")).toBe(true);
    expect(matchesFilter(dec("12"), price, "10..12")).toBe(true);
    expect(matchesFilter(dec("12"), price, "in:1,12")).toBe(true);
  });

  it("handles NULL like the engines", () => {
    expect(matchesFilter(NULL, name, "null")).toBe(true);
    expect(matchesFilter(NULL, name, "!null")).toBe(false);
    expect(matchesFilter(NULL, name, "!abc")).toBe(false);
    expect(matchesFilter(text("x"), name, "")).toBe(true);
  });

  it("matches nothing for an invalid filter", () => {
    expect(matchesFilter(dec("1"), price, ">abc")).toBe(false);
  });
});
