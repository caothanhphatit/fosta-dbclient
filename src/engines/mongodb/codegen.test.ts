import { describe, expect, it } from "vitest";
import { parseMongoShell } from "../../lang/mongo/shell";
import { exportFindQuery, exportPipeline, formatValue, generateCode, type CodeLanguage, type CodegenRequest } from "./codegen";
import { EMPTY_QUERY } from "./queryModel";

const TARGET = { database: "shop", collection: "orders" };

/** mongosh text to canonical Extended JSON, failing the test when it does not parse. */
function ejson(text: string): string {
  const result = parseMongoShell(text);
  if (!result.ok) throw new Error(`${result.error} at ${result.position}`);
  return result.json;
}

function code(language: CodeLanguage, request: CodegenRequest): string {
  const result = generateCode(language, request);
  if (!result.ok) throw new Error(result.error);
  return result.code;
}

function value(language: CodeLanguage, shell: string): string {
  const result = formatValue(language, ejson(shell));
  if (!result.ok) throw new Error(result.error);
  return result.code;
}

const FIND: CodegenRequest = {
  ...TARGET,
  kind: "find",
  find: {
    filter: ejson('{ status: "paid", customerId: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"), createdAt: { $gte: ISODate("2024-01-31T00:00:00Z") } }'),
    projection: ejson("{ _id: 0, total: 1 }"),
    sort: ejson("{ createdAt: -1, _id: 1 }"),
    collation: ejson('{ locale: "en" }'),
    skip: 10,
    limit: 20,
  },
};

const PIPELINE = [
  '{ $match: { status: "paid", amount: { $gt: NumberDecimal("10.50") } } }',
  '{ $group: { _id: "$customerId", total: { $sum: "$amount" }, count: { $sum: 1 } } }',
  "{ $sort: { total: -1 } }",
  "{ $limit: 5 }",
].map(ejson);

const AGGREGATE: CodegenRequest = { ...TARGET, kind: "aggregate", pipeline: PIPELINE, options: { allowDiskUse: true, maxTimeMS: 5000 } };

describe("find", () => {
  it("mongosh", () => {
    expect(code("mongosh", FIND)).toMatchInlineSnapshot(`
      "db.getSiblingDB("shop")
        .getCollection("orders")
        .find({
          status: "paid",
          customerId: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"),
          createdAt: { $gte: ISODate("2024-01-31T00:00:00.000Z") }
        }, { _id: 0, total: 1 })
        .sort({ createdAt: -1, _id: 1 })
        .collation({ locale: "en" })
        .skip(10)
        .limit(20);"
    `);
  });

  it("Node.js", () => {
    expect(code("node", FIND)).toMatchInlineSnapshot(`
      "import { MongoClient, ObjectId } from "mongodb";

      const client = new MongoClient(process.env.MONGODB_URI ?? "mongodb://localhost:27017");

      async function main() {
        try {
          const collection = client.db("shop").collection("orders");
          const documents = await collection
            .find({
              status: "paid",
              customerId: new ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"),
              createdAt: { $gte: new Date("2024-01-31T00:00:00.000Z") }
            })
            .project({ _id: 0, total: 1 })
            .sort({ createdAt: -1, _id: 1 })
            .collation({ locale: "en" })
            .skip(10)
            .limit(20)
            .toArray();
          console.log(documents);
        } finally {
          await client.close();
        }
      }

      main().catch(console.error);"
    `);
  });

  it("Python", () => {
    expect(code("python", FIND)).toMatchInlineSnapshot(`
      "import os
      from datetime import datetime, timezone

      from bson import ObjectId
      from pymongo import MongoClient

      client = MongoClient(os.environ.get("MONGODB_URI", "mongodb://localhost:27017"))
      collection = client["shop"]["orders"]

      cursor = collection.find(
          {
              "status": "paid",
              "customerId": ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"),
              "createdAt": {"$gte": datetime(2024, 1, 31, tzinfo=timezone.utc)},
          },
          projection={"_id": 0, "total": 1},
          sort=[("createdAt", -1), ("_id", 1)],
          skip=10,
          limit=20,
          collation={"locale": "en"},
      )
      for document in cursor:
          print(document)"
    `);
  });

  it("an empty find", () => {
    const request: CodegenRequest = { ...TARGET, kind: "find" };
    expect(code("mongosh", request)).toMatchInlineSnapshot(`
      "db.getSiblingDB("shop")
        .getCollection("orders")
        .find({});"
    `);
    expect(code("node", request)).toMatchInlineSnapshot(`
      "import { MongoClient } from "mongodb";

      const client = new MongoClient(process.env.MONGODB_URI ?? "mongodb://localhost:27017");

      async function main() {
        try {
          const collection = client.db("shop").collection("orders");
          const documents = await collection
            .find({})
            .toArray();
          console.log(documents);
        } finally {
          await client.close();
        }
      }

      main().catch(console.error);"
    `);
    expect(code("python", request)).toMatchInlineSnapshot(`
      "import os

      from pymongo import MongoClient

      client = MongoClient(os.environ.get("MONGODB_URI", "mongodb://localhost:27017"))
      collection = client["shop"]["orders"]

      cursor = collection.find({})
      for document in cursor:
          print(document)"
    `);
  });
});

describe("aggregate", () => {
  it("mongosh", () => {
    expect(code("mongosh", AGGREGATE)).toMatchInlineSnapshot(`
      "db.getSiblingDB("shop")
        .getCollection("orders")
        .aggregate([
          { $match: { status: "paid", amount: { $gt: NumberDecimal("10.50") } } },
          {
            $group: {
              _id: "$customerId",
              total: { $sum: "$amount" },
              count: { $sum: 1 }
            }
          },
          { $sort: { total: -1 } },
          { $limit: 5 }
        ], { allowDiskUse: true, maxTimeMS: 5000 });"
    `);
  });

  it("Node.js", () => {
    expect(code("node", AGGREGATE)).toMatchInlineSnapshot(`
      "import { Decimal128, MongoClient } from "mongodb";

      const client = new MongoClient(process.env.MONGODB_URI ?? "mongodb://localhost:27017");

      async function main() {
        try {
          const collection = client.db("shop").collection("orders");
          const documents = await collection
            .aggregate([
              {
                $match: {
                  status: "paid",
                  amount: { $gt: Decimal128.fromString("10.50") }
                }
              },
              {
                $group: {
                  _id: "$customerId",
                  total: { $sum: "$amount" },
                  count: { $sum: 1 }
                }
              },
              { $sort: { total: -1 } },
              { $limit: 5 }
            ], { allowDiskUse: true, maxTimeMS: 5000 })
            .toArray();
          console.log(documents);
        } finally {
          await client.close();
        }
      }

      main().catch(console.error);"
    `);
  });

  it("Python", () => {
    expect(code("python", AGGREGATE)).toMatchInlineSnapshot(`
      "import os

      from bson import Decimal128
      from pymongo import MongoClient

      client = MongoClient(os.environ.get("MONGODB_URI", "mongodb://localhost:27017"))
      collection = client["shop"]["orders"]

      cursor = collection.aggregate(
          [
              {"$match": {"status": "paid", "amount": {"$gt": Decimal128("10.50")}}},
              {
                  "$group": {
                      "_id": "$customerId",
                      "total": {"$sum": "$amount"},
                      "count": {"$sum": 1},
                  },
              },
              {"$sort": {"total": -1}},
              {"$limit": 5},
          ],
          allowDiskUse=True,
          maxTimeMS=5000,
      )
      for document in cursor:
          print(document)"
    `);
  });

  it("breaks long stages one entry per line", () => {
    const stage = ejson(
      '{ $lookup: { from: "customers", localField: "customerId", foreignField: "_id", as: "customer", pipeline: [{ $project: { name: 1, email: 1 } }] } }',
    );
    const request: CodegenRequest = { ...TARGET, kind: "aggregate", pipeline: [stage], options: { collation: ejson('{ locale: "fr", strength: 2 }') } };
    expect(code("mongosh", request)).toMatchInlineSnapshot(`
      "db.getSiblingDB("shop")
        .getCollection("orders")
        .aggregate([
          {
            $lookup: {
              from: "customers",
              localField: "customerId",
              foreignField: "_id",
              as: "customer",
              pipeline: [{ $project: { name: 1, email: 1 } }]
            }
          }
        ], { collation: { locale: "fr", strength: 2 } });"
    `);
    expect(code("python", request)).toMatchInlineSnapshot(`
      "import os

      from pymongo import MongoClient

      client = MongoClient(os.environ.get("MONGODB_URI", "mongodb://localhost:27017"))
      collection = client["shop"]["orders"]

      cursor = collection.aggregate(
          [
              {
                  "$lookup": {
                      "from": "customers",
                      "localField": "customerId",
                      "foreignField": "_id",
                      "as": "customer",
                      "pipeline": [{"$project": {"name": 1, "email": 1}}],
                  },
              },
          ],
          collation={"locale": "fr", "strength": 2},
      )
      for document in cursor:
          print(document)"
    `);
  });
});

describe("wrapper types", () => {
  const CASES: [string, string][] = [
    ["objectId", 'ObjectId("65A1F0C2E4B0A1B2C3D4E5F6")'],
    ["date", 'ISODate("2024-01-31T12:34:56.789Z")'],
    ["date at midnight", 'ISODate("2024-01-31")'],
    ["date before 1970", 'ISODate("1969-07-20T20:17:00Z")'],
    ["date past year 9999", "new Date(253402300800000)"],
    ["int64", 'NumberLong("9223372036854775807")'],
    ["int32 wrapper", "NumberInt(7)"],
    ["int32", "42"],
    ["double", "1.5"],
    ["whole double past int32", "3000000000"],
    ["double past 2^53", "1e300"],
    ["NaN", "NaN"],
    ["Infinity", "Infinity"],
    ["-Infinity", "-Infinity"],
    ["decimal", 'NumberDecimal("19.99")'],
    ["binary", 'BinData(0, "AQID")'],
    ["user binary", 'BinData(128, "AQID")'],
    ["UUID", 'UUID("123e4567-e89b-12d3-a456-426614174000")'],
    ["legacy UUID", 'BinData(3, "EjRWeJASNFZ4kBI0VniQEg==")'],
    ["timestamp", "Timestamp({ t: 1700000000, i: 3 })"],
    ["regex", "/^ab+c/i"],
    ["regex with a slash", 'RegExp("a/b", "m")'],
    ["regex JavaScript cannot read", 'RegExp("(?i)a", "x")'],
    ["empty regex", 'RegExp("")'],
    ["minKey", "MinKey()"],
    ["maxKey", "MaxKey()"],
    ["literals", "[true, false, null]"],
  ];

  const table = (language: CodeLanguage) => CASES.map(([name, shell]) => `${name}: ${value(language, shell)}`).join("\n");

  it("in mongosh", () => {
    expect(table("mongosh")).toMatchInlineSnapshot(`
      "objectId: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6")
      date: ISODate("2024-01-31T12:34:56.789Z")
      date at midnight: ISODate("2024-01-31T00:00:00.000Z")
      date before 1970: ISODate("1969-07-20T20:17:00.000Z")
      date past year 9999: new Date(253402300800000)
      int64: NumberLong("9223372036854775807")
      int32 wrapper: NumberInt(7)
      int32: 42
      double: 1.5
      whole double past int32: 3000000000
      double past 2^53: 1e+300
      NaN: NaN
      Infinity: Infinity
      -Infinity: -Infinity
      decimal: NumberDecimal("19.99")
      binary: BinData(0, "AQID")
      user binary: BinData(128, "AQID")
      UUID: UUID("123e4567-e89b-12d3-a456-426614174000")
      legacy UUID: BinData(3, "EjRWeJASNFZ4kBI0VniQEg==")
      timestamp: Timestamp({ t: 1700000000, i: 3 })
      regex: /^ab+c/i
      regex with a slash: RegExp("a/b", "m")
      regex JavaScript cannot read: BSONRegExp("(?i)a", "x")
      empty regex: RegExp("")
      minKey: MinKey()
      maxKey: MaxKey()
      literals: [true, false, null]"
    `);
  });

  it("in Node.js", () => {
    expect(table("node")).toMatchInlineSnapshot(`
      "objectId: new ObjectId("65a1f0c2e4b0a1b2c3d4e5f6")
      date: new Date("2024-01-31T12:34:56.789Z")
      date at midnight: new Date("2024-01-31T00:00:00.000Z")
      date before 1970: new Date("1969-07-20T20:17:00.000Z")
      date past year 9999: new Date(253402300800000)
      int64: Long.fromString("9223372036854775807")
      int32 wrapper: 7
      int32: 42
      double: 1.5
      whole double past int32: 3000000000
      double past 2^53: 1e+300
      NaN: NaN
      Infinity: Infinity
      -Infinity: -Infinity
      decimal: Decimal128.fromString("19.99")
      binary: Binary.createFromBase64("AQID", 0)
      user binary: Binary.createFromBase64("AQID", 128)
      UUID: new UUID("123e4567-e89b-12d3-a456-426614174000")
      legacy UUID: Binary.createFromBase64("EjRWeJASNFZ4kBI0VniQEg==", 3)
      timestamp: new Timestamp({ t: 1700000000, i: 3 })
      regex: new BSONRegExp("^ab+c", "i")
      regex with a slash: new BSONRegExp("a/b", "m")
      regex JavaScript cannot read: new BSONRegExp("(?i)a", "x")
      empty regex: new BSONRegExp("")
      minKey: new MinKey()
      maxKey: new MaxKey()
      literals: [true, false, null]"
    `);
  });

  it("in Python", () => {
    expect(table("python")).toMatchInlineSnapshot(`
      "objectId: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6")
      date: datetime(2024, 1, 31, 12, 34, 56, 789000, tzinfo=timezone.utc)
      date at midnight: datetime(2024, 1, 31, tzinfo=timezone.utc)
      date before 1970: datetime(1969, 7, 20, 20, 17, tzinfo=timezone.utc)
      date past year 9999: DatetimeMS(253402300800000)
      int64: Int64(9223372036854775807)
      int32 wrapper: 7
      int32: 42
      double: 1.5
      whole double past int32: 3000000000.0
      double past 2^53: 1e+300
      NaN: float("nan")
      Infinity: float("inf")
      -Infinity: float("-inf")
      decimal: Decimal128("19.99")
      binary: Binary(bytes.fromhex("010203"))
      user binary: Binary(bytes.fromhex("010203"), 128)
      UUID: Binary.from_uuid(UUID("123e4567-e89b-12d3-a456-426614174000"))
      legacy UUID: Binary(bytes.fromhex("12345678901234567890123456789012"), 3)
      timestamp: Timestamp(1700000000, 3)
      regex: Regex("^ab+c", "i")
      regex with a slash: Regex("a/b", "m")
      regex JavaScript cannot read: Regex("(?i)a", "x")
      empty regex: Regex("")
      minKey: MinKey()
      maxKey: MaxKey()
      literals: [True, False, None]"
    `);
  });

  it("spells whole doubles so that the driver keeps them doubles", () => {
    const json = '{"a":{"$numberDouble":"2"},"b":2.0,"c":{"$numberDouble":"-0.0"}}';
    expect(formatValue("mongosh", json)).toEqual({ ok: true, code: "{ a: Double(2), b: Double(2), c: Double(-0) }" });
    expect(formatValue("node", json)).toEqual({ ok: true, code: "{ a: new Double(2), b: new Double(2), c: new Double(-0) }" });
    expect(formatValue("python", json)).toEqual({ ok: true, code: '{"a": 2.0, "b": 2.0, "c": -0.0}' });
  });

  it("reads relaxed dates and an integer past int32 as an int64", () => {
    expect(formatValue("mongosh", '[{"$date":"2024-01-31T00:00:00Z"},{"$date":0},5000000000]')).toEqual({
      ok: true,
      code: '[\n  ISODate("2024-01-31T00:00:00.000Z"),\n  ISODate("1970-01-01T00:00:00.000Z"),\n  NumberLong("5000000000")\n]',
    });
  });

  it("refuses Extended JSON it cannot write", () => {
    expect(formatValue("node", '{"$code":"function() {}"}')).toEqual({ ok: false, error: "Extended JSON $code values cannot be exported" });
    expect(formatValue("node", '{"$oid":"nope"}')).toEqual({ ok: false, error: "Invalid $oid value" });
    expect(formatValue("node", "{")).toMatchObject({ ok: false });
    expect(generateCode("python", { ...TARGET, kind: "aggregate", pipeline: ["[]"] })).toEqual({ ok: false, error: "Stage 1 must be a document" });
  });

  it("imports only what the script uses", () => {
    const request = (filter: string): CodegenRequest => ({ ...TARGET, kind: "find", find: { filter: ejson(filter) } });
    expect(code("node", request("{ a: 1 }")).split("\n")[0]).toBe('import { MongoClient } from "mongodb";');
    expect(code("node", request('{ a: NumberLong("1"), b: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"), c: /x/ }')).split("\n")[0]).toBe(
      'import { BSONRegExp, Long, MongoClient, ObjectId } from "mongodb";',
    );
    expect(code("python", request("{ a: 1 }")).split("\n").slice(0, 3)).toEqual(["import os", "", "from pymongo import MongoClient"]);
    expect(
      code("python", request('{ a: ISODate("2024-01-31"), b: UUID("123e4567-e89b-12d3-a456-426614174000"), c: new Date(-62198755200000) }'))
        .split("\n")
        .slice(0, 7),
    ).toMatchInlineSnapshot(`
        [
          "import os",
          "from datetime import datetime, timezone",
          "from uuid import UUID",
          "",
          "from bson import Binary",
          "from bson.datetime_ms import DatetimeMS",
          "from pymongo import MongoClient",
        ]
      `);
  });
});

describe("strings and keys", () => {
  const SHELL = String.raw`{ "naïve café": "日本語 🍣", "a.b": "quote \" backslash \\ tab \t newline \n", "": "\u0001\u007f\u2028", "$x": 1, "1": "first?", _id: '\ud800' }`;

  it("escape per language and keep unicode readable", () => {
    expect(value("mongosh", SHELL)).toMatchInlineSnapshot(`
      "{
        "naïve café": "日本語 🍣",
        "a.b": "quote \\" backslash \\\\ tab \\t newline \\n",
        "": "\\u0001\\u007f\\u2028",
        $x: 1,
        "1": "first?",
        _id: "\\ud800"
      }"
    `);
    expect(value("node", SHELL)).toMatchInlineSnapshot(`
      "{
        "naïve café": "日本語 🍣",
        "a.b": "quote \\" backslash \\\\ tab \\t newline \\n",
        "": "\\u0001\\u007f\\u2028",
        $x: 1,
        "1": "first?",
        _id: "\\ud800"
      }"
    `);
    expect(value("python", SHELL)).toMatchInlineSnapshot(`
      "{
          "naïve café": "日本語 🍣",
          "a.b": "quote \\" backslash \\\\ tab \\t newline \\n",
          "": "\\x01\\x7f\\u2028",
          "$x": 1,
          "1": "first?",
          "_id": "\\ud800",
      }"
    `);
  });

  it("keep key order, integer-like keys included", () => {
    const json = '{"z":1,"2":2,"a":3,"1":4}';
    expect(formatValue("mongosh", json)).toEqual({ ok: true, code: '{ z: 1, "2": 2, a: 3, "1": 4 }' });
    expect(formatValue("python", json)).toEqual({ ok: true, code: '{"z": 1, "2": 2, "a": 3, "1": 4}' });
  });

  it("quote database and collection names", () => {
    const request: CodegenRequest = { database: 'we"ird', collection: "a\\b ü", kind: "find" };
    expect(code("mongosh", request).split("\n").slice(0, 2)).toEqual(['db.getSiblingDB("we\\"ird")', '  .getCollection("a\\\\b ü")']);
    expect(code("python", request)).toContain('collection = client["we\\"ird"]["a\\\\b ü"]');
  });
});

describe("mongosh round trip", () => {
  /** The pipeline text between `.aggregate(` and the closing `);` of a mongosh command without options. */
  const pipelineOf = (command: string) => command.slice(command.indexOf(".aggregate(") + ".aggregate(".length, command.lastIndexOf(");"));

  it("reads back to the same Extended JSON", () => {
    const stages = [
      String.raw`{ $match: { _id: ObjectId("65a1f0c2e4b0a1b2c3d4e5f6"), at: { $gte: ISODate("2024-01-31T12:34:56.789Z"), $lt: new Date(-1000) } } }`,
      String.raw`{ $match: { n: NumberLong("-9223372036854775808"), i: NumberInt(3), d: 1.25, big: 3000000000, huge: 9.007199254740994e15, tiny: 1e-7 } }`,
      String.raw`{ $match: { nan: NaN, inf: Infinity, ninf: -Infinity, dec: NumberDecimal("1E+10"), u: UUID("123e4567-e89b-12d3-a456-426614174000") } }`,
      String.raw`{ $match: { bin: BinData(5, "AAEC"), ts: Timestamp(1, 2), re: /a\/b[/]c/ims, re2: RegExp("x/y", "i"), lo: MinKey(), hi: MaxKey() } }`,
      String.raw`{ $project: { "naïve": "日本語 🍣 \" \\ \n \u0000 \x7f", "a.b": [1, [2, {}], []], z: true, y: null, "1": 0 } }`,
    ].map(ejson);
    const command = code("mongosh", { ...TARGET, kind: "aggregate", pipeline: stages });
    const back = parseMongoShell(pipelineOf(command), { expect: "array" });
    expect(back).toEqual({ ok: true, json: `[${stages.join(",")}]` });
  });

  it("holds for the find parts too", () => {
    const command = code("mongosh", FIND);
    const find = /\.find\((.*), (\{ _id: 0, total: 1 \})\)/s.exec(command);
    expect(find && parseMongoShell(find[1]).ok && ejson(find[1])).toBe(FIND.find?.filter);
  });
});

describe("helpers for the UI", () => {
  it("export the query bar's find", () => {
    const state = { ...EMPTY_QUERY, filter: 'status: "paid"', sort: "{ at: -1 }", limit: 50 };
    expect(exportFindQuery("mongosh", state, TARGET)).toEqual({
      ok: true,
      code: 'db.getSiblingDB("shop")\n  .getCollection("orders")\n  .find({ status: "paid" })\n  .sort({ at: -1 })\n  .limit(50);',
    });
    expect(exportFindQuery("python", { ...state, filter: "status:" }, TARGET)).toMatchObject({ ok: false, error: expect.stringMatching(/^filter: /) });
  });

  it("export a pipeline typed as mongosh stages", () => {
    const result = exportPipeline("node", TARGET, ["{ $match: { a: 1 } }"]);
    expect(result.ok && result.code).toContain("      .aggregate([\n        { $match: { a: 1 } }\n      ])\n      .toArray();");
    expect(exportPipeline("node", TARGET, ["{ $match: { a: 1 } }", "{ $limit: }"])).toMatchObject({ ok: false, error: expect.stringMatching(/^Stage 2: /) });
  });
});
