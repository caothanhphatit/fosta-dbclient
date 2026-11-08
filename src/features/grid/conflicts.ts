/**
 * The pure side of the conflict dialog: reads the structured `CONFLICT` error of `apply_edits` and lays each conflicting
 * row out as cells of "When opened" (what the user loaded), "Now" (the row in the database) and "Yours" (what the save
 * tried to write).
 */
import type { ConflictError, Field, ObjectRef, RowConflict, Value } from "../../core/contract";
import { errorCode } from "../../core/errors";
import { displayValue, valuesEqual } from "../../core/values";

export interface ConflictPayload {
  object: ObjectRef | null;
  message: string;
  conflicts: RowConflict[];
  /** See `ConflictError.compensated`. */
  compensated: boolean | null;
}

/** The conflict payload of an `apply_edits` rejection, or null when it is another error. */
export function conflictPayload(error: unknown): ConflictPayload | null {
  if (errorCode(error) !== "CONFLICT") return null;
  const source = error as Partial<ConflictError>;
  if (!Array.isArray(source.conflicts)) return null;
  return {
    object: source.object ?? null,
    message: typeof source.message === "string" ? source.message : "The rows changed since they were loaded.",
    conflicts: source.conflicts,
    compensated: typeof source.compensated === "boolean" ? source.compensated : null,
  };
}

export interface ConflictCell {
  column: string;
  /** Undefined when the column is not part of that side (e.g. "Yours" of a delete). */
  original: Value | undefined;
  current: Value | undefined;
  attempted: Value | undefined;
  /** Someone else changed it: "Now" differs from "When opened". */
  changedNow: boolean;
  /** The save changes it: "Yours" differs from "When opened". */
  changedByYou: boolean;
}

export interface ConflictRow {
  /** Index of the change in the set that was sent (`RowConflict.index`). */
  index: number;
  op: RowConflict["op"];
  key: Field[];
  /** "id = 7", "CustomerID = 3, Line = 2". */
  keyText: string;
  /** The row no longer exists. */
  missing: boolean;
  cells: ConflictCell[];
  /** Nothing in the involved columns differs: another column or the row version changed. */
  otherChange: boolean;
}

function lookup(fields: Field[] | null | undefined, column: string): Value | undefined {
  return fields?.find((field) => field.column === column)?.value;
}

function differs(a: Value | undefined, b: Value | undefined): boolean {
  if (a === undefined || b === undefined) return false;
  return !valuesEqual(a, b);
}

/** A cell for display: NULL spelled out. */
export function cellText(value: Value | undefined): string {
  if (value === undefined) return "";
  return displayValue(value) ?? "NULL";
}

export function keyText(key: Field[]): string {
  return key.map((field) => `${field.column} = ${cellText(field.value)}`).join(", ");
}

/**
 * One row per conflict. The involved columns are the ones the change carried (`original` and `attempted`), plus, when
 * `loaded` gives the whole row as the grid holds it, every column that someone else changed since.
 */
export function conflictRows(conflicts: RowConflict[], loaded: (conflict: RowConflict) => Field[] | null = () => null): ConflictRow[] {
  return conflicts.map((conflict) => {
    const whole = loaded(conflict);
    const columns: string[] = [];
    const add = (column: string) => {
      if (!columns.includes(column)) columns.push(column);
    };
    for (const field of conflict.original) add(field.column);
    for (const field of conflict.attempted) add(field.column);
    if (whole && conflict.current) {
      for (const field of whole) if (differs(field.value, lookup(conflict.current, field.column))) add(field.column);
    }
    const cells = columns.map((column): ConflictCell => {
      const original = lookup(conflict.original, column) ?? lookup(whole, column);
      const current = conflict.current ? lookup(conflict.current, column) : undefined;
      const attempted = lookup(conflict.attempted, column);
      return { column, original, current, attempted, changedNow: differs(current, original), changedByYou: differs(attempted, original) };
    });
    const missing = conflict.current === null;
    return {
      index: conflict.index,
      op: conflict.op,
      key: conflict.key,
      keyText: keyText(conflict.key),
      missing,
      cells,
      otherChange: !missing && !cells.some((cell) => cell.changedNow),
    };
  });
}
