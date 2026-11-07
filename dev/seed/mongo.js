// Dummy event data for MongoDB, run by the image's init hook as root.
const events = db.getSiblingDB("events");

const devices = ["web", "ios", "android", "desktop"];
const pages = ["/", "/products", "/cart", "/checkout", "/account", "/search"];
const cities = ["Ha Noi", "Ho Chi Minh", "Da Nang", "Can Tho", "Hue"];

const users = [];
for (let i = 1; i <= 500; i += 1) {
  users.push({
    _id: i,
    name: `User ${i}`,
    email: `user${i}@example.com`,
    city: cities[i % cities.length],
    plan: i % 10 === 0 ? "pro" : "free",
    tags: i % 3 === 0 ? ["beta", "newsletter"] : ["newsletter"],
    signedUpAt: new Date(Date.UTC(2025, 0, 1) + i * 86400000 * 0.9),
  });
}
events.users.insertMany(users);

const batch = [];
for (let i = 1; i <= 20000; i += 1) {
  const type = i % 25 === 0 ? "purchase" : i % 7 === 0 ? "add_to_cart" : "page_view";
  const doc = {
    type,
    userId: 1 + ((i * 37) % 500),
    device: devices[i % devices.length],
    page: pages[(i * 5) % pages.length],
    at: new Date(Date.UTC(2026, 9, 1) + i * 133000),
    session: { id: `s-${Math.floor(i / 6)}`, durationMs: (i * 811) % 600000 },
  };
  if (type === "purchase") doc.order = { total: 50 + ((i * 13) % 950), items: 1 + (i % 4), currency: "USD" };
  if (type === "add_to_cart") doc.productId = 1 + ((i * 101) % 1240);
  batch.push(doc);
}
events.clicks.insertMany(batch);

events.clicks.createIndex({ type: 1, at: -1 });
events.clicks.createIndex({ userId: 1 });
events.users.createIndex({ email: 1 }, { unique: true });
