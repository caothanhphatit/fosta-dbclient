/**
 * The dev mock's validation operations, like src-tauri/src/engines/mongo/validation.rs: `mongo.validation` (the rules of
 * a collection, seeded with a demo validator on shop.orders), `mongo.setValidation` (level and action checked, an
 * empty validator removing the rules) and `mongo.validationCheck` (the documents a validator refuses, counted and
 * sampled). A simple `$jsonSchema` checker covers bsonType, required, properties, items, enum, minimum / maximum,
 * minLength / maxLength, pattern, minItems / maxItems and additionalProperties; query operators go through the mock's
 * matcher; anything else is "not evaluated in mock" (it passes, and the result says so).
 */
import { mockError } from "../errors";

type Json = unknown;
type Doc = Record<string, Json>;

interface Rules {
  validator: Doc | null;
  validationLevel: string;
  validationAction: string;
}

const LEVELS = ["off", "strict", "moderate"];
const ACTIONS = ["error", "warn"];
const MAX_SAMPLES = 20;
const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value);

/** Demo rules, so the section opens on a validator that some documents fail (returned orders). */
const DEMO: Record<string, Rules> = {
  "shop.orders": {
    validator: {
      $jsonSchema: {
        bsonType: "object",
        required: ["customerId", "status", "items"],
        properties: {
          status: { enum: ["new", "paid", "shipped"], description: "The order's state" },
          items: {
            bsonType: "array",
            minItems: 1,
            items: { bsonType: "object", required: ["sku", "qty"], properties: { qty: { bsonType: "int", minimum: 1 } } },
          },
        },
      },
    },
    validationLevel: "moderate",
    validationAction: "warn",
  },
};

const store = new Map<string, Rules>();

export interface ValidationHost {
  /** The documents of a collection (Extended JSON), null when there is no such collection. */
  documents(database: string, collection: string): Doc[] | null;
  /** Whether a document matches a query document (throws for operators the mock does not evaluate). */
  matches(document: Doc, filter: Doc): boolean;
}

function parse(args: Doc, key: string): Json {
  const value = args[key];
  if (typeof value !== "string") return value ?? undefined;
  if (!value.trim()) return undefined;
  try {
    return JSON.parse(value) as Json;
  } catch (error) {
    throw mockError("ENGINE_ERROR", `${key} must be a JSON document (${error instanceof Error ? error.message : String(error)})`);
  }
}

function rulesOf(host: ValidationHost, args: Doc): { id: string; rules: Rules; documents: Doc[]; collection: string } {
  const database = String(args.database ?? "");
  const collection = String(args.collection ?? "");
  const id = `${database}.${collection}`;
  const documents = host.documents(database, collection);
  if (!documents) throw mockError("ENGINE_ERROR", `ns does not exist: ${id}`);
  let rules = store.get(id);
  if (!rules) {
    rules = structuredClone(DEMO[id] ?? { validator: null, validationLevel: "strict", validationAction: "error" });
    store.set(id, rules);
  }
  return { id, rules, documents, collection };
}

export function getValidation(host: ValidationHost, args: Doc): Json {
  const { rules, collection } = rulesOf(host, args);
  return {
    ...structuredClone(rules),
    type: "collection",
    statement: `db.getCollectionInfos({ name: ${JSON.stringify(collection)} })[0].options`,
    elapsedMs: 2,
  };
}

export function setValidation(host: ValidationHost, args: Doc): Json {
  const { rules, collection } = rulesOf(host, args);
  const validator = parse(args, "validator");
  if (validator !== undefined && !isDoc(validator)) throw mockError("ENGINE_ERROR", "validator must be a JSON document");
  const choice = (key: string, allowed: string[]) => {
    const value = typeof args[key] === "string" ? String(args[key]).trim() : args[key];
    if (value === undefined || value === null || value === "") return undefined;
    if (typeof value !== "string" || !allowed.includes(value))
      throw mockError("ENGINE_ERROR", `${key} must be ${allowed.join(", ")}, not ${JSON.stringify(value)}`);
    return value;
  };
  const level = choice("validationLevel", LEVELS);
  const action = choice("validationAction", ACTIONS);
  if (validator === undefined && !level && !action) throw mockError("ENGINE_ERROR", "Give a validator, a validationLevel or a validationAction");
  const command: Doc = { collMod: collection };
  if (validator !== undefined) {
    command.validator = validator;
    rules.validator = Object.keys(validator).length > 0 ? validator : null;
  }
  if (level) rules.validationLevel = command.validationLevel = level;
  if (action) rules.validationAction = command.validationAction = action;
  return {
    validator: validator ?? null,
    validationLevel: level ?? null,
    validationAction: action ?? null,
    statement: `db.runCommand(${JSON.stringify(command)})`,
    elapsedMs: 4,
  };
}

/** The BSON type of an Extended JSON value. */
function typeOf(value: Json): string {
  if (value === null) return "null";
  if (typeof value === "string") return "string";
  if (typeof value === "boolean") return "bool";
  if (typeof value === "number") return Number.isInteger(value) && Math.abs(value) <= 2_147_483_647 ? "int" : "double";
  if (Array.isArray(value)) return "array";
  if (!isDoc(value)) return "undefined";
  if ("$oid" in value) return "objectId";
  if ("$date" in value) return "date";
  if ("$numberDecimal" in value) return "decimal";
  if ("$numberLong" in value) return "long";
  if ("$numberInt" in value) return "int";
  if ("$numberDouble" in value) return "double";
  if ("$binary" in value || "$uuid" in value) return "binData";
  if ("$regularExpression" in value) return "regex";
  if ("$timestamp" in value) return "timestamp";
  return "object";
}

const NUMBER_TYPES = ["int", "long", "double", "decimal"];

function numberOf(value: Json): number | null {
  if (typeof value === "number") return value;
  if (!isDoc(value)) return null;
  for (const key of ["$numberInt", "$numberLong", "$numberDouble", "$numberDecimal"]) if (key in value) return Number(value[key]);
  return null;
}

const same = (a: Json, b: Json) => JSON.stringify(a) === JSON.stringify(b) || (numberOf(a) !== null && numberOf(a) === numberOf(b));

const HANDLED = new Set([
  "bsonType",
  "type",
  "required",
  "properties",
  "items",
  "enum",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "minLength",
  "maxLength",
  "pattern",
  "minItems",
  "maxItems",
  "additionalProperties",
  "description",
  "title",
]);

/** Whether `value` passes `schema`; keywords the mock does not check are added to `skipped`. */
function passes(value: Json, schema: Json, skipped: Set<string>): boolean {
  if (!isDoc(schema)) return true;
  for (const key of Object.keys(schema)) if (!HANDLED.has(key)) skipped.add(key);
  const type = typeOf(value);
  for (const key of ["bsonType", "type"]) {
    if (schema[key] === undefined) continue;
    const wanted = (Array.isArray(schema[key]) ? schema[key] : [schema[key]]) as Json[];
    const ok = wanted.some(
      (name) =>
        name === type ||
        (name === "number" && NUMBER_TYPES.includes(type)) ||
        (name === "integer" && type === "int") ||
        (name === "boolean" && type === "bool"),
    );
    if (!ok) return false;
  }
  if (Array.isArray(schema.enum) && !schema.enum.some((option) => same(option, value))) return false;
  const number = NUMBER_TYPES.includes(type) ? numberOf(value) : null;
  if (number !== null) {
    if (typeof schema.minimum === "number" && (schema.exclusiveMinimum === true ? number <= schema.minimum : number < schema.minimum)) return false;
    if (typeof schema.maximum === "number" && (schema.exclusiveMaximum === true ? number >= schema.maximum : number > schema.maximum)) return false;
  }
  if (typeof value === "string") {
    if (typeof schema.minLength === "number" && value.length < schema.minLength) return false;
    if (typeof schema.maxLength === "number" && value.length > schema.maxLength) return false;
    if (typeof schema.pattern === "string" && !new RegExp(schema.pattern, "u").test(value)) return false;
  }
  if (Array.isArray(value)) {
    if (typeof schema.minItems === "number" && value.length < schema.minItems) return false;
    if (typeof schema.maxItems === "number" && value.length > schema.maxItems) return false;
    if (isDoc(schema.items) && !value.every((item) => passes(item, schema.items, skipped))) return false;
  }
  if (type === "object" && isDoc(value)) {
    if (Array.isArray(schema.required) && !schema.required.every((name) => typeof name === "string" && name in value)) return false;
    const properties = isDoc(schema.properties) ? schema.properties : {};
    for (const [name, rule] of Object.entries(properties)) if (name in value && !passes(value[name], rule, skipped)) return false;
    if (schema.additionalProperties === false && Object.keys(value).some((name) => !(name in properties))) return false;
  }
  return true;
}

/** Whether a document passes a validator; what the mock could not evaluate goes to `skipped` (and passes). */
function validates(host: ValidationHost, document: Doc, validator: Doc, skipped: Set<string>): boolean {
  return Object.entries(validator).every(([key, condition]) => {
    if (key === "$jsonSchema") return passes(document, condition, skipped);
    if (key === "$and") return (condition as Doc[]).every((part) => validates(host, document, part, skipped));
    if (key === "$or") return (condition as Doc[]).some((part) => validates(host, document, part, skipped));
    if (key === "$nor") return !(condition as Doc[]).some((part) => validates(host, document, part, skipped));
    try {
      return host.matches(document, { [key]: condition });
    } catch {
      skipped.add(key);
      return true;
    }
  });
}

export function validationCheck(host: ValidationHost, args: Doc): Json {
  const { documents, collection } = rulesOf(host, args);
  const validator = parse(args, "validator");
  if (!isDoc(validator)) throw mockError("ENGINE_ERROR", "validator is required");
  const limit = Math.min(Math.max(Number(args.limit ?? 5) || 5, 1), MAX_SAMPLES);
  if (Object.keys(validator).length === 0) return { failing: 0, capped: false, total: documents.length, samples: [], statement: "", elapsedMs: 1, note: null };
  const skipped = new Set<string>();
  const failing = documents.filter((document) => !validates(host, document, validator, skipped));
  return {
    failing: failing.length,
    capped: false,
    total: documents.length,
    samples: failing.slice(0, limit),
    statement: `db.getCollection(${JSON.stringify(collection)}).find(${JSON.stringify({ $nor: [validator] })})`,
    elapsedMs: 6,
    note: skipped.size > 0 ? `Not evaluated in mock: ${[...skipped].join(", ")} (treated as passing).` : null,
  };
}
