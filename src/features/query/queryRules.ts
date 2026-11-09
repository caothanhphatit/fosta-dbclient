/** Small decisions of the query tab, kept pure so they are unit-tested: its default database, DDL, unsaved text. */

/**
 * The database a new query tab starts in: the one asked for, else the only one listed, else (an engine without
 * separate databases, such as SQLite with `main` and its attached files) the first one listed.
 */
export function defaultDatabase(preferred: string | null, databases: string[], multipleDatabases: boolean): string | null {
  if (preferred) return preferred;
  if (databases.length === 1 || !multipleDatabases) return databases[0] ?? null;
  return null;
}

/** Statements that change which tables and columns exist (SQL DDL, MongoDB collection and index commands). */
const SCHEMA_CHANGE =
  /\b(create|alter|drop|rename|truncate|comment\s+on|attach|detach|createCollection|createIndexes?|dropIndexes?|dropDatabase|renameCollection|collMod)\b/i;

/**
 * Whether `text` may have changed the schema, so the completion names are loaded again after it ran. A keyword check
 * outside comments: a false positive costs one extra reload.
 */
export function changesSchema(text: string): boolean {
  const withoutComments = text.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(--|#|\/\/)[^\n]*/g, " ");
  return SCHEMA_CHANGE.test(withoutComments);
}

/**
 * Whether a query tab's text would be lost on close: something is typed, and it is not the text that last ran (or that
 * the tab was opened with, such as the SELECT a table's "New query" writes).
 */
export function hasUnsavedText(text: string, baseline: string): boolean {
  return text.trim() !== "" && text !== baseline;
}
