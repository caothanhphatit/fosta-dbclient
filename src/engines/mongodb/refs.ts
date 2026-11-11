/**
 * Inferred references, without DOM: a field named like a reference (`customerId`, `customer_id`, `tagIds`) that holds
 * ObjectIds probably points at a document of another collection of the same database. Which one is found by probing
 * (`mongo.resolveRef` with a sampled id): first the collections whose name matches the field (`customerId` →
 * `customers`), then others, at most `MAX_PROBES` per field. The answer per field is cached, and so are the documents
 * read; the UI labels these links "inferred", since nothing in MongoDB declares them.
 */
import { bsonType } from "./bson";
import { isNested, type Doc, type Json } from "./documents";

/** Collections probed per field, and fields probed per page. */
export const MAX_PROBES = 4;
export const MAX_REF_FIELDS = 8;
/** Referenced documents kept per workspace tab. */
const DOCUMENT_CACHE = 200;

const REF_NAME = /^(.+?)(?:_ids?|Ids?|IDs?)$/;

/** People-like field names that usually point at a users collection. */
const USER_ALIASES = new Set(["owner", "author", "creator", "createdby", "updatedby", "modifiedby", "assignee", "member", "reviewer", "account"]);

/** The last part of a dotted path with array indexes left out (`items.0.productId` → `items.productId`). */
export function refPath(path: readonly string[]): string {
  return path.filter((part) => !/^\d+$/.test(part)).join(".");
}

/** Whether the last field name of `path` reads like a reference (not `_id` itself). */
export function isRefName(path: string): boolean {
  const name = path.slice(path.lastIndexOf(".") + 1);
  return name !== "_id" && REF_NAME.test(name);
}

const normal = (name: string) => name.toLowerCase().replace(/[_\-\s]/g, "");

function plurals(word: string): string[] {
  const forms = [word, `${word}s`, `${word}es`];
  if (/[^aeiou]y$/.test(word)) forms.push(`${word.slice(0, -1)}ies`);
  return forms;
}

/** The names a field's target collection may have, normalised (lower case, no `_`): `customerId` → customer(s). */
export function targetNames(path: string): string[] {
  const name = path.slice(path.lastIndexOf(".") + 1);
  const base = REF_NAME.exec(name)?.[1];
  if (!base) return [];
  const names = new Set(plurals(normal(base)));
  // `parentCategoryId` → categories, `billing_address_id` → addresses.
  const words = base.split(/_|(?=[A-Z])/).filter(Boolean);
  const last = words[words.length - 1];
  if (words.length > 1 && last.length >= 3) for (const form of plurals(normal(last))) names.add(form);
  if (USER_ALIASES.has(normal(base))) for (const form of plurals("user")) names.add(form);
  return [...names];
}

/**
 * The collections to probe for `path`, in order: those whose name matches, then the others (not `self` unless its
 * name matches, never `system.*`), at most `MAX_PROBES`.
 */
export function candidateCollections(path: string, collections: readonly string[], self: string, max = MAX_PROBES): string[] {
  const names = targetNames(path);
  const usable = collections.filter((name) => !name.startsWith("system."));
  const matching = usable.filter((name) => names.includes(normal(name)));
  const others = usable.filter((name) => name !== self && !matching.includes(name)).sort();
  return [...matching, ...others].slice(0, max);
}

/**
 * The fields of `documents` that may be references: a reference-like name holding an ObjectId (or ObjectIds in an
 * array), each with the first id seen (to probe with). Bounded to `MAX_REF_FIELDS`, in first-seen order.
 */
export function refFields(documents: readonly Doc[], max = MAX_REF_FIELDS): Map<string, Json> {
  const found = new Map<string, Json>();
  const walk = (value: Json, path: string[], depth: number) => {
    if (found.size >= max || depth > 6) return;
    if (Array.isArray(value)) {
      for (const item of value.slice(0, 20)) walk(item, path, depth + 1);
      return;
    }
    if (bsonType(value) === "objectId") {
      const dotted = refPath(path);
      if (dotted && isRefName(dotted) && !found.has(dotted)) found.set(dotted, value);
      return;
    }
    if (!isNested(value)) return;
    for (const [key, inner] of Object.entries(value as Doc)) walk(inner, [...path, key], depth + 1);
  };
  for (const document of documents.slice(0, 200)) walk(document, [], 0);
  return found;
}

/** Reads one document by `_id` in a collection of the same database; null when there is none. */
export type ResolveRef = (collection: string, id: Json) => Promise<Doc | null>;

/** The inferred target of each reference field, and the referenced documents, cached for the tab. */
export class RefResolver {
  private readonly targets = new Map<string, Promise<string | null>>();
  private readonly settled = new Map<string, string | null>();
  private readonly documents = new Map<string, Promise<Doc | null>>();

  constructor(
    private readonly resolve: ResolveRef,
    private readonly collections: () => readonly string[] | Promise<readonly string[]>,
    private readonly self: string,
    private readonly maxProbes = MAX_PROBES,
  ) {}

  /** The target collection of `path` once known (undefined while not probed or probing, null when none was found). */
  known(path: string): string | null | undefined {
    return this.settled.get(path);
  }

  /** Probes `path` with `sample` (once per path; later calls share the answer). */
  target(path: string, sample: Json): Promise<string | null> {
    let pending = this.targets.get(path);
    if (!pending) {
      pending = this.probe(path, sample).then((target) => {
        this.settled.set(path, target);
        return target;
      });
      this.targets.set(path, pending);
    }
    return pending;
  }

  /** The document `id` of `collection` (cached; a failed read is not kept). */
  document(collection: string, id: Json): Promise<Doc | null> {
    const key = `${collection}\u0000${JSON.stringify(id)}`;
    const cached = this.documents.get(key);
    if (cached) return cached;
    const read = this.resolve(collection, id).catch((error: unknown) => {
      this.documents.delete(key);
      throw error;
    });
    this.documents.set(key, read);
    if (this.documents.size > DOCUMENT_CACHE) this.documents.delete(this.documents.keys().next().value as string);
    return read;
  }

  private async probe(path: string, sample: Json): Promise<string | null> {
    const collections = await this.collections();
    for (const collection of candidateCollections(path, collections, this.self, this.maxProbes)) {
      try {
        if (await this.document(collection, sample)) return collection;
      } catch {
        // A collection that cannot be read (refused, no rights) is not the target.
      }
    }
    return null;
  }
}
