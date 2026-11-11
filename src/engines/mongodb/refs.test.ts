import { describe, expect, it } from "vitest";
import type { Doc, Json } from "./documents";
import { candidateCollections, isRefName, refFields, refPath, RefResolver, targetNames } from "./refs";

const id = (n: number) => ({ $oid: `68e783b8c0ffee00000000${String(n).padStart(2, "0")}` });

describe("reference-like fields", () => {
  it("knows reference names", () => {
    for (const name of ["customerId", "customer_id", "tagIds", "items.productId", "ownerID"]) expect(isRefName(name)).toBe(true);
    for (const name of ["_id", "id", "identity", "items._id", "paid"]) expect(isRefName(name)).toBe(false);
    expect(refPath(["items", "0", "productId"])).toBe("items.productId");
  });

  it("guesses the collection names a field points at", () => {
    expect(targetNames("customerId")).toEqual(expect.arrayContaining(["customer", "customers"]));
    expect(targetNames("parentCategoryId")).toEqual(expect.arrayContaining(["parentcategories", "categories"]));
    expect(targetNames("billing_address_id")).toEqual(expect.arrayContaining(["addresses"]));
    expect(targetNames("ownerId")).toEqual(expect.arrayContaining(["users"]));
    expect(targetNames("status")).toEqual([]);
  });

  it("probes matching collections first, then others, bounded", () => {
    const collections = ["orders", "Customers", "products", "system.views", "reviews", "audit", "zeta"];
    expect(candidateCollections("customerId", collections, "orders")).toEqual(["Customers", "audit", "products", "reviews"]);
    expect(candidateCollections("order_item_id", ["order_items", "orders"], "orders", 2)).toEqual(["order_items"]);
    expect(candidateCollections("parentId", ["categories"], "categories")).toEqual([]);
    expect(candidateCollections("parentCategoryId", ["categories"], "categories")).toEqual(["categories"]);
  });

  it("finds ObjectId fields with reference names, through arrays, with a sample id", () => {
    const documents: Doc[] = [
      { _id: id(1), customerId: id(2), status: "paid", items: [{ productId: id(3), qty: 1 }], tagIds: [id(4), id(5)], noteId: "text" },
      { _id: id(6), customerId: id(7) },
    ];
    const fields = refFields(documents);
    expect([...fields.keys()]).toEqual(["customerId", "items.productId", "tagIds"]);
    expect(fields.get("customerId")).toEqual(id(2));
    expect(fields.get("tagIds")).toEqual(id(4));
    expect(refFields(documents, 1).size).toBe(1);
  });
});

describe("RefResolver", () => {
  const setup = (found: Record<string, Json[]>) => {
    const calls: string[] = [];
    const resolver = new RefResolver(
      async (collection, value) => {
        calls.push(collection);
        if (collection === "broken") throw new Error("not allowed");
        return (found[collection] ?? []).some((item) => JSON.stringify(item) === JSON.stringify(value)) ? { _id: value as Doc } : null;
      },
      () => ["orders", "broken", "customers", "products"],
      "orders",
    );
    return { resolver, calls };
  };

  it("takes the first collection holding the sampled id, once per field", async () => {
    const { resolver, calls } = setup({ customers: [id(2)] });
    expect(resolver.known("customerId")).toBeUndefined();
    const [first, second] = await Promise.all([resolver.target("customerId", id(2)), resolver.target("customerId", id(2))]);
    expect([first, second]).toEqual(["customers", "customers"]);
    expect(resolver.known("customerId")).toBe("customers");
    expect(calls).toEqual(["customers"]);
  });

  it("skips collections that fail, and remembers that none matched", async () => {
    const { resolver, calls } = setup({ products: [id(9)] });
    expect(await resolver.target("sellerId", id(9))).toBe("products");
    expect(calls).toEqual(["broken", "customers", "products"]);
    expect(await resolver.target("buyerId", id(1))).toBeNull();
    expect(resolver.known("buyerId")).toBeNull();
  });

  it("caches the documents read, not the failures", async () => {
    const { resolver, calls } = setup({ customers: [id(2)] });
    expect(await resolver.document("customers", id(2))).toEqual({ _id: id(2) });
    expect(await resolver.document("customers", id(2))).toEqual({ _id: id(2) });
    expect(calls).toEqual(["customers"]);
    await expect(resolver.document("broken", id(2))).rejects.toThrow("not allowed");
    await expect(resolver.document("broken", id(2))).rejects.toThrow("not allowed");
    expect(calls).toEqual(["customers", "broken", "broken"]);
  });
});
