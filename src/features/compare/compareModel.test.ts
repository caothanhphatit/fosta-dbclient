import { describe, expect, it } from "vitest";
import type { DiffItem, DiffStatus, Risk } from "../../core/contract";
import { presetSettings } from "../safety/safety";
import {
  carrySelection,
  changeRows,
  changeSummary,
  comparableEngines,
  compareKeyParts,
  compareTitle,
  defaultOptions,
  defaultSelection,
  displayRisk,
  groupCheckState,
  groupItems,
  includedIds,
  parseOptions,
  safetyNotice,
  scriptFileName,
  setGroup,
  summary,
  swapped,
  type CompareRequest,
} from "./compareModel";

function item(name: string, status: DiffStatus, kind: "table" | "view" = "table", risk: Risk = "none", changes = 0): DiffItem {
  return {
    id: `${kind}:${name}`,
    kind,
    name,
    status,
    changes: Array.from({ length: changes }, (_, index) => ({
      part: "column",
      name: `c${index}`,
      what: "modified" as const,
      fields: [],
      risk: "none" as const,
      note: null,
      possibleRename: null,
    })),
    risk,
    sourceDdl: null,
    targetDdl: null,
  };
}

const ITEMS = [
  item("orders", "different", "table", "dataLoss", 2),
  item("coupons", "onlyInSource"),
  item("audit_log", "onlyInTarget"),
  item("v_legacy", "onlyInTarget", "view"),
  item("order_items", "identical"),
  item("v_totals", "different", "view", "none", 1),
];

describe("selection", () => {
  it("checks differences and source-only objects, never drops or identical ones", () => {
    expect([...defaultSelection(ITEMS)].sort()).toEqual(["table:coupons", "table:orders", "view:v_totals"]);
  });

  it("keeps the user's choice while an item's status stays, and defaults the rest", () => {
    const before = new Set(["table:orders", "table:audit_log"]);
    const after = [
      item("orders", "different"),
      item("audit_log", "onlyInTarget"),
      item("coupons", "onlyInSource"),
      item("invoices", "onlyInSource"),
      item("v_totals", "identical", "view"),
    ];
    // coupons was unchecked by the user and stays so; invoices is new and takes the default.
    expect([...carrySelection(ITEMS, before, after)].sort()).toEqual(["table:audit_log", "table:invoices", "table:orders"]);
    // Swapped sides: source-only becomes target-only, so its choice resets to unchecked.
    const swappedItems = [item("coupons", "onlyInTarget")];
    expect([...carrySelection(ITEMS, new Set(["table:coupons"]), swappedItems)]).toEqual([]);
  });

  it("lists the included ids in diff order, without identical items", () => {
    const selection = new Set(["view:v_totals", "table:orders", "table:order_items"]);
    expect(includedIds(ITEMS, selection)).toEqual(["table:orders", "view:v_totals"]);
  });
});

describe("groups", () => {
  it("orders the groups and hides identical objects unless asked", () => {
    expect(groupItems(ITEMS, "", false).map((group) => [group.status, group.total])).toEqual([
      ["different", 2],
      ["onlyInSource", 1],
      ["onlyInTarget", 2],
    ]);
    expect(groupItems(ITEMS, "", true).map((group) => group.status)).toContain("identical");
  });

  it("filters by name but keeps the totals", () => {
    const groups = groupItems(ITEMS, "ORDER", true);
    const different = groups.find((group) => group.status === "different");
    expect(different?.items.map((entry) => entry.name)).toEqual(["orders"]);
    expect(different?.total).toBe(2);
    expect(groups.find((group) => group.status === "onlyInSource")?.items).toEqual([]);
  });

  it("toggles a whole group and reports its checkbox state", () => {
    const [different, , onlyInTarget] = groupItems(ITEMS, "", false);
    let selection = defaultSelection(ITEMS);
    expect(groupCheckState(onlyInTarget, selection)).toBe("none");
    selection = setGroup(selection, onlyInTarget, true);
    expect(groupCheckState(onlyInTarget, selection)).toBe("all");
    selection.delete("table:orders");
    expect(groupCheckState(different, selection)).toBe("some");
    expect(groupCheckState(different, setGroup(selection, different, false))).toBe("none");
  });

  it("summarizes rows and the toolbar", () => {
    expect(changeSummary(ITEMS[0])).toBe("2 changes");
    expect(changeSummary(ITEMS[5])).toBe("1 change");
    expect(changeSummary(ITEMS[1])).toBe("new");
    expect(changeSummary(ITEMS[2])).toBe("drop");
    expect(summary(ITEMS, defaultSelection(ITEMS))).toBe("5 differences · 3 selected");
    expect(summary([item("a", "identical")], new Set())).toBe("No differences");
    expect(summary([], new Set())).toBe("Nothing to compare");
  });

  it("marks dropping a table as data loss whatever the item's risk", () => {
    expect(displayRisk(item("audit_log", "onlyInTarget"))).toBe("dataLoss");
    expect(displayRisk(item("v_legacy", "onlyInTarget", "view"))).toBe("none");
    expect(displayRisk(item("orders", "different", "table", "review"))).toBe("review");
  });
});

describe("details", () => {
  it("has one row per changed field, with the note on the first", () => {
    const entry: DiffItem = {
      ...item("orders", "different"),
      changes: [
        {
          part: "column",
          name: "status",
          what: "modified",
          fields: [
            { field: "type", source: "character varying(20)", target: "character varying(50)" },
            { field: "default", source: null, target: "'new'" },
          ],
          risk: "dataLoss",
          note: "The type change may lose data",
          possibleRename: null,
        },
        { part: "foreignKey", name: "orders_fk", what: "removed", fields: [], risk: "none", note: null, possibleRename: null },
      ],
    };
    expect(changeRows(entry)).toEqual([
      ["Column", "status", "Modified", "type", "character varying(20)", "character varying(50)", "Data loss: The type change may lose data"],
      ["Column", "status", "Modified", "default", "", "'new'", ""],
      ["Foreign key", "orders_fk", "Removed", "", "", "", ""],
    ]);
  });
});

describe("requests", () => {
  const request: CompareRequest = {
    source: { connectionId: "dev", database: "shop", schema: "public" },
    target: { connectionId: "stage", database: "shop", schema: "public" },
    options: defaultOptions(),
  };
  const names = (id: string) => ({ dev: "Local", stage: "Staging" })[id] ?? id;

  it("titles by schema, or by connection when the schema names are the same", () => {
    expect(compareTitle(request, names)).toBe("Local → Staging");
    expect(compareTitle({ ...request, target: { ...request.target, schema: "sales" } }, names)).toBe("public → sales");
    expect(compareTitle({ ...request, target: { ...request.source, database: "shop2" } }, names)).toBe("shop.public → shop2.public");
  });

  it("keys a pair by both sides, so a swap is another tab", () => {
    expect(compareKeyParts(request)).not.toEqual(compareKeyParts(swapped(request)));
    expect(swapped(swapped(request))).toEqual(request);
  });

  it("compares the same engine only", () => {
    expect(comparableEngines("postgres", "postgres")).toBe(true);
    expect(comparableEngines("postgres", "mssql")).toBe(false);
  });

  it("reads saved options leniently", () => {
    expect(parseOptions(null)).toEqual(defaultOptions());
    const parsed = parseOptions({ ignoreComments: true, exclude: ["tmp_*"], transactional: false, detectRenames: "yes", bogus: 1 });
    expect(parsed).toMatchObject({ ignoreComments: true, exclude: ["tmp_*"], transactional: false, detectRenames: true });
    expect(parsed).not.toHaveProperty("bogus");
  });

  it("names a saved script after the time and the target database", () => {
    expect(scriptFileName(new Date(2026, 10, 22, 9, 5), "shop staging")).toBe("202611220905_sync_shop_staging.sql");
  });
});

describe("the target's safety notice", () => {
  it("blocks for Read-only and Production", () => {
    const [notice] = safetyNotice(presetSettings("readOnly"), true);
    expect(notice.tone).toBe("danger");
    expect(notice.text).toContain("Production");
    expect(safetyNotice(presetSettings("readOnly"), false)[0].text).toContain("block DDL");
  });

  it("explains the confirmations and timeout under Guarded", () => {
    const notices = safetyNotice(presetSettings("guarded"), false);
    expect(notices.map((notice) => notice.tone)).toEqual(["info", "info"]);
    expect(notices[0].text).toBe("Running it on the target asks for a confirmation for DDL and a typed confirmation for DROP.");
    expect(notices[1].text).toMatch(/Statements stop after \d+ s/);
  });

  it("warns when the workspace runs one statement at a time", () => {
    const custom = { ...presetSettings("custom"), allowMultiStatement: false };
    expect(safetyNotice(custom, false).some((notice) => notice.tone === "warning")).toBe(true);
  });

  it("says nothing more than the DROP confirmation under Open, and nothing without rules", () => {
    expect(safetyNotice(presetSettings("open"), false).map((notice) => notice.text)).toEqual(["Running it on the target asks for a confirmation for DROP."]);
    expect(safetyNotice(null, false)).toEqual([]);
  });
});
