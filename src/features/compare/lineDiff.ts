/**
 * A line diff (Myers' O(ND) algorithm, the greedy forward variant with a trace for the backtrack) and the side-by-side
 * rows the compare tab draws from it. Pure, no DOM; small on purpose (DDL of one table or view is a few dozen lines).
 */

export type DiffOp = { kind: "equal"; a: string; b: string } | { kind: "delete"; a: string } | { kind: "insert"; b: string };

/** The edit script that turns `a` into `b`, line by line: equal lines kept, deletions before insertions in a hunk. */
export function diffLines(a: readonly string[], b: readonly string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  const max = n + m;
  const offset = max + 1;
  // v[k + offset] = the furthest x reached on diagonal k; `trace` keeps a copy per edit distance d for the backtrack.
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];
  let found = max === 0;
  for (let d = 0; d <= max && !found; d += 1) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      const down = k === -d || (k !== d && v[k - 1 + offset] < v[k + 1 + offset]);
      let x = down ? v[k + 1 + offset] : v[k - 1 + offset] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      v[k + offset] = x;
      if (x >= n && y >= m) {
        found = true;
        break;
      }
    }
  }
  if (max === 0) return [];
  // Walk back from (n, m) through each recorded front.
  const ops: DiffOp[] = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d >= 0; d -= 1) {
    const front = trace[d];
    const k = x - y;
    const down = k === -d || (k !== d && front[k - 1 + offset] < front[k + 1 + offset]);
    const previousK = down ? k + 1 : k - 1;
    const previousX = d === 0 ? 0 : front[previousK + offset];
    const previousY = previousX - previousK;
    while (x > previousX && y > previousY) {
      x -= 1;
      y -= 1;
      ops.push({ kind: "equal", a: a[x], b: b[y] });
    }
    if (d === 0) break;
    if (down) {
      y -= 1;
      ops.push({ kind: "insert", b: b[y] });
    } else {
      x -= 1;
      ops.push({ kind: "delete", a: a[x] });
    }
  }
  ops.reverse();
  return normalizeHunks(ops);
}

/** Within each run of changes, deletions come first, so a changed line pairs with its replacement. */
function normalizeHunks(ops: DiffOp[]): DiffOp[] {
  const result: DiffOp[] = [];
  let deletes: DiffOp[] = [];
  let inserts: DiffOp[] = [];
  const flush = () => {
    result.push(...deletes, ...inserts);
    deletes = [];
    inserts = [];
  };
  for (const op of ops) {
    if (op.kind === "equal") {
      flush();
      result.push(op);
    } else if (op.kind === "delete") deletes.push(op);
    else inserts.push(op);
  }
  flush();
  return result;
}

/** One row of the side-by-side view: `left` from the target (old), `right` from the source (new). */
export interface SideRow {
  kind: "equal" | "change" | "removed" | "added";
  left: string | null;
  right: string | null;
  /** 1-based line numbers; null on the side that has no line. */
  leftNumber: number | null;
  rightNumber: number | null;
}

/**
 * Side-by-side rows of `diffLines(left, right)`: in each hunk the deleted and inserted lines are paired as changes, and
 * what is left over is a removal (left only) or an addition (right only).
 */
export function sideBySide(left: readonly string[], right: readonly string[]): SideRow[] {
  const ops = diffLines(left, right);
  const rows: SideRow[] = [];
  let leftNumber = 0;
  let rightNumber = 0;
  let index = 0;
  while (index < ops.length) {
    const op = ops[index];
    if (op.kind === "equal") {
      leftNumber += 1;
      rightNumber += 1;
      rows.push({ kind: "equal", left: op.a, right: op.b, leftNumber, rightNumber });
      index += 1;
      continue;
    }
    const removed: string[] = [];
    const added: string[] = [];
    while (index < ops.length && ops[index].kind !== "equal") {
      const change = ops[index];
      if (change.kind === "delete") removed.push(change.a);
      else if (change.kind === "insert") added.push(change.b);
      index += 1;
    }
    for (let line = 0; line < Math.max(removed.length, added.length); line += 1) {
      const hasLeft = line < removed.length;
      const hasRight = line < added.length;
      if (hasLeft) leftNumber += 1;
      if (hasRight) rightNumber += 1;
      rows.push({
        kind: hasLeft && hasRight ? "change" : hasLeft ? "removed" : "added",
        left: hasLeft ? removed[line] : null,
        right: hasRight ? added[line] : null,
        leftNumber: hasLeft ? leftNumber : null,
        rightNumber: hasRight ? rightNumber : null,
      });
    }
  }
  return rows;
}

/** Splits DDL text into lines (no trailing empty line); null or empty text has none. */
export function textLines(text: string | null): string[] {
  if (!text) return [];
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}
