/** CSV parsing for imports: pure functions, no DOM. */

/** The separator (comma, semicolon, tab or pipe) that splits the first lines into the most fields. */
export function detectDelimiter(text: string): string {
  const sample = text.split(/\r?\n/).slice(0, 5).join("\n");
  const counts = [",", ";", "\t", "|"].map((separator) => ({ separator, count: parseCsv(sample, separator)[0]?.length ?? 0 }));
  return counts.sort((a, b) => b.count - a.count)[0].separator;
}

/** RFC 4180 CSV: quoted fields may hold separators, newlines and doubled quotes. */
export function parseCsv(text: string, separator: string): string[][] {
  const records: string[][] = [];
  let record: string[] = [];
  let value = "";
  let quoted = false;
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (quoted) {
      if (character === '"' && text[index + 1] === '"') {
        value += '"';
        index += 1;
      } else if (character === '"') quoted = false;
      else value += character;
    } else if (character === '"' && value === "") quoted = true;
    else if (character === separator) {
      record.push(value);
      value = "";
    } else if (character === "\n" || character === "\r") {
      if (character === "\r" && text[index + 1] === "\n") index += 1;
      record.push(value);
      records.push(record);
      record = [];
      value = "";
    } else value += character;
  }
  if (value !== "" || record.length) {
    record.push(value);
    records.push(record);
  }
  return records;
}
