/** How SQLite declared types read in the definition view: the declared name and the affinity SQLite gives it. */

export type Affinity = "INTEGER" | "TEXT" | "BLOB" | "REAL" | "NUMERIC";

/** The column affinity of a declared type, by the rules of section 3.1 of SQLite's datatype documentation. */
export function affinity(declared: string): Affinity {
  const type = declared.toUpperCase();
  if (type.includes("INT")) return "INTEGER";
  if (type.includes("CHAR") || type.includes("CLOB") || type.includes("TEXT")) return "TEXT";
  if (type.trim() === "" || type.includes("BLOB")) return "BLOB";
  if (type.includes("REAL") || type.includes("FLOA") || type.includes("DOUB")) return "REAL";
  return "NUMERIC";
}

/** `VARCHAR(100) · TEXT`; a column without a declared type holds whatever is stored (`any`). */
export function typeLabel(declared: string): string {
  const name = declared.trim();
  return name === "" ? "any" : `${name} · ${affinity(name)}`;
}
