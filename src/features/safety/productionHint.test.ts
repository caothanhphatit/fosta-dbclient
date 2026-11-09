import { describe, expect, it } from "vitest";
import { looksLikeProduction, productionWorkspace, shouldOfferProductionMove } from "./productionHint";
import type { ConnectionProfile, Workspace } from "../../core/types";

const profile = (fields: Partial<ConnectionProfile>): ConnectionProfile => ({ id: "c1", name: "Shop", host: "db.local", ...fields }) as ConnectionProfile;
const workspace = (icon: string): Workspace => ({ id: icon, name: icon, icon, color: "blue", connections: [], tasks: [] }) as unknown as Workspace;

describe("production hint", () => {
  it("spots production words in name, host, database or URL", () => {
    expect(looksLikeProduction(profile({ name: "Shop prod" }))).toBe(true);
    expect(looksLikeProduction(profile({ host: "orders-prd.db.example.com" }))).toBe(true);
    expect(looksLikeProduction(profile({ database: "shop_production" }))).toBe(true);
    expect(looksLikeProduction(profile({ url: "postgres://u@live-db:5432/x" }))).toBe(true);
  });

  it("ignores words that only contain the letters", () => {
    expect(looksLikeProduction(profile({ name: "Products catalog" }))).toBe(false);
    expect(looksLikeProduction(profile({ host: "olive.internal", database: "productivity" }))).toBe(false);
    expect(looksLikeProduction(profile({ name: "Staging" }))).toBe(false);
  });

  it("offers the move only outside Production and once per connection", () => {
    const prod = profile({ name: "Billing PROD" });
    expect(shouldOfferProductionMove(prod, workspace("laptop"), new Set())).toBe(true);
    expect(shouldOfferProductionMove(prod, workspace("shield"), new Set())).toBe(false);
    expect(shouldOfferProductionMove(prod, workspace("laptop"), new Set(["c1"]))).toBe(false);
    expect(productionWorkspace([workspace("laptop"), workspace("shield")])?.icon).toBe("shield");
    expect(productionWorkspace([workspace("laptop")])).toBeUndefined();
  });
});
