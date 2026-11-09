import { describe, expect, it } from "vitest";
import { disambiguateTitles, tabTooltip } from "./tabTitles";

describe("disambiguateTitles", () => {
  it("leaves unique titles alone", () => {
    expect(
      disambiguateTitles([
        { title: "orders", connection: "Staging" },
        { title: "Query 1", connection: "Staging" },
      ]),
    ).toEqual(["orders", "Query 1"]);
  });

  it("adds the connection when the same title comes from two connections", () => {
    expect(
      disambiguateTitles([
        { title: "pain_big", connection: "MariaDB 11", database: "test" },
        { title: "pain_big", connection: "MariaDB 10", database: "test" },
        { title: "Query 1", connection: "MariaDB 11" },
      ]),
    ).toEqual(["pain_big · MariaDB 11", "pain_big · MariaDB 10", "Query 1"]);
  });

  it("adds the database when that is what differs", () => {
    expect(
      disambiguateTitles([
        { title: "orders", connection: "Staging", database: "shop" },
        { title: "orders", connection: "Staging", database: "archive" },
        { title: "orders", connection: "Production", database: "shop" },
      ]),
    ).toEqual(["orders · Staging · shop", "orders · Staging · archive", "orders · Production"]);
  });

  it("keeps the title when nothing tells the tabs apart", () => {
    expect(disambiguateTitles([{ title: "Logs" }, { title: "Logs" }])).toEqual(["Logs", "Logs"]);
  });
});

describe("tabTooltip", () => {
  it("names the connection first", () => {
    expect(tabTooltip({ title: "orders", connection: "Staging", subtitle: "shop.orders" })).toBe("Staging · shop.orders");
    expect(tabTooltip({ title: "Deploy · logs" })).toBe("Deploy · logs");
  });
});
