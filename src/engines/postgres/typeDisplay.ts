/** How PostgreSQL type names read in the definition view: the short aliases `psql` users know, arrays as `T[]`. */

const SHORT: Record<string, string> = {
  "character varying": "varchar",
  character: "char",
  "timestamp with time zone": "timestamptz",
  "timestamp without time zone": "timestamp",
  "time with time zone": "timetz",
  "time without time zone": "time",
  "double precision": "float8",
  real: "float4",
  bigint: "int8",
  integer: "int4",
  smallint: "int2",
  boolean: "bool",
  "bit varying": "varbit",
};

/** `character varying(200)` as `varchar(200)`, `timestamp with time zone[]` as `timestamptz[]`; other names as they are. */
export function shortType(typeName: string): string {
  const array = /^(.*)\[\]$/.exec(typeName);
  if (array) return `${shortType(array[1])}[]`;
  const parts = /^([^(]+?)(\(.*\))?$/.exec(typeName.trim());
  if (!parts) return typeName;
  return `${SHORT[parts[1]] ?? parts[1]}${parts[2] ?? ""}`;
}

/** The labels of an enum from the `labels` fact of its description (`new, paid, shipped`). */
export function enumLabels(extra: [string, string][]): string[] {
  const labels = extra.find(([name]) => name === "labels")?.[1];
  return labels
    ? labels
        .split(",")
        .map((label) => label.trim())
        .filter(Boolean)
    : [];
}
