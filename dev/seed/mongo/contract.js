// Contract-test data for MongoDB (src-tauri/src/contract/testing). The Rust fixture builds the same documents itself
// before each suite (database fosta_contract); this script is for a fresh container or a manual reset:
//   docker exec -i db-studio-dev-mongo-1 mongosh -u root -p studio-root --quiet < dev/seed/mongo/contract.js
const contract = db.getSiblingDB("fosta_contract");
contract.contract_items.drop();
contract.contract_children.drop();
const items = [];
for (let i = 1; i <= 1000; i++) {
  items.push({
    _id: i,
    name: "item " + String(i).padStart(4, "0"),
    price: NumberDecimal((i * 1.25).toFixed(2)),
    active: i % 2 === 0,
    born: new Date(Date.UTC(2020, 0, 1 + i)),
    seen: new Date(Date.UTC(2026, 0, 1, 8, i, 0, 250)),
    meta: { n: i, tags: ["a", "b"] },
    blob_data: BinData(0, Buffer.from(i.toString(16).padStart(8, "0"), "hex").toString("base64")),
    note: i % 3 === 0 ? null : "note " + i,
  });
}
contract.contract_items.insertMany(items);
contract.contract_items.createIndex({ name: 1 }, { unique: true });
const children = [];
for (let i = 1; i <= 200; i++) children.push({ _id: i, item_id: i, label: "child " + i });
contract.contract_children.insertMany(children);
