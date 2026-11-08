/**
 * Table import (CSV, TSV, JSON, NDJSON) and export (CSV, CSV for Excel, JSON,
 * SQL INSERT, Markdown) for the data tab. Export can cover the whole table
 * (all rows matching the current filter), fetched page by page. Files are
 * written to Downloads by the backend; imports run as batched INSERTs, each
 * batch in one transaction.
 */
import { capabilities } from "../../core/capabilities";
import { api, errorText } from "../../core/api";
import { checkbox, field, openModal, textInput, toast } from "../../core/dialogs";
import { formatNumber, h } from "../../core/dom";
import { driverInfo } from "../../core/drivers";
import { detectDelimiter, parseCsv } from "./csv";
import { exportDelimited, exportRows } from "./exporters";
import { sqlDialect, type SqlDialect } from "../../core/sql";
import { ConfirmationSession } from "../safety/confirmationDialog";
import type { BrowseRequest, Cell, ColumnMeta, RowChange, TableTarget } from "../../core/types";

const FETCH_PAGE = 1000;
const IMPORT_BATCH = 500;
const PREVIEW_ROWS = 6;
const SKIP = "__skip__";
/** Byte order mark: Excel needs it to read UTF-8; imports drop it. */
const BOM = String.fromCharCode(0xfeff);
const LEADING_BOM = new RegExp(`^${BOM}`);

type FileFormat = "csv" | "excel" | "json" | "insert" | "markdown";

const FORMATS: { value: FileFormat; label: string; extension: string }[] = [
  { value: "csv", label: "CSV", extension: "csv" },
  { value: "excel", label: "CSV for Excel (UTF-8 with BOM, ; separated)", extension: "csv" },
  { value: "json", label: "JSON", extension: "json" },
  { value: "insert", label: "SQL INSERT statements", extension: "sql" },
  { value: "markdown", label: "Markdown table", extension: "md" },
];

export interface ExportSource {
  target: TableTarget;
  columns: ColumnMeta[];
  /** Rows of the page on screen. */
  pageRows: Cell[][];
  /** Everything needed to fetch all rows matching the current filter and sort. */
  request: Omit<BrowseRequest, "offset" | "limit">;
  total: number;
}

export function exportDialog(source: ExportSource): void {
  const format = h("select", { class: "input" }, ...FORMATS.map((item) => h("option", { value: item.value }, item.label)));
  const scopeAll = h("input", { type: "radio", name: "scope", checked: true });
  const scopePage = h("input", { type: "radio", name: "scope" });
  const fileName = textInput(`${source.target.table}.csv`, { class: "input mono" });
  const progress = h("p", { class: "test-status" });
  format.addEventListener("change", () => {
    const extension = FORMATS.find((item) => item.value === format.value)?.extension ?? "txt";
    fileName.value = fileName.value.replace(/\.[^.]*$/, "") + `.${extension}`;
  });
  const filtered = source.request.filter ? " matching the filter" : "";
  const body = h(
    "div",
    { class: "form-grid" },
    field("Format", format),
    h(
      "div",
      { class: "field" },
      h("span", { class: "field-label" }, "Rows"),
      h("label", { class: "checkbox" }, scopeAll, h("span", {}, `All ${formatNumber(source.total)} rows${filtered}`)),
      h("label", { class: "checkbox" }, scopePage, h("span", {}, `This page only (${formatNumber(source.pageRows.length)} rows)`)),
    ),
    field("File name", fileName, capabilities().browserDownloads ? "Downloaded by your browser" : "Saved to your Downloads folder"),
    progress,
  );
  openModal(
    `Export ${source.target.table}`,
    body,
    [
      { label: "Cancel", run: () => undefined },
      {
        label: "Export",
        primary: true,
        run: async () => {
          const rows = scopePage.checked
            ? source.pageRows
            : await fetchAll(source, (done) => (progress.textContent = `Reading rows… ${formatNumber(done)} / ${formatNumber(source.total)}`));
          progress.textContent = "Writing file…";
          const text = render(format.value as FileFormat, source.columns, rows, source.target.table, sqlDialect(source.target.driver));
          const path = await api.saveDownload(fileName.value.trim() || `${source.target.table}.csv`, text, null);
          savedDialog(`Exported ${formatNumber(rows.length)} rows`, path);
        },
      },
    ],
    { width: 480 },
  );
}

async function fetchAll(source: ExportSource, onProgress: (done: number) => void): Promise<Cell[][]> {
  const rows: Cell[][] = [];
  for (let offset = 0; offset < source.total || offset === 0; offset += FETCH_PAGE) {
    const page = await api.browseTable({ ...source.request, offset, limit: FETCH_PAGE });
    rows.push(...page.rows);
    onProgress(rows.length);
    if (page.rows.length < FETCH_PAGE) break;
  }
  return rows;
}

function render(format: FileFormat, columns: ColumnMeta[], rows: Cell[][], table: string, dialect: SqlDialect): string {
  switch (format) {
    case "excel":
      // Excel on a Vietnamese/European locale expects ";" and only reads UTF-8 with a BOM.
      return `${BOM}${exportDelimited(columns, rows, ";")}`;
    case "csv":
    case "json":
    case "insert":
    case "markdown":
      return exportRows(format, columns, rows, table, dialect);
  }
}

/** Small result dialog with a "Show in folder" button. */
export function savedDialog(title: string, path: string): void {
  if (!capabilities().revealFile) {
    // Web edition: the browser has the file (`path` is its name); its own download UI shows where.
    toast(`${title}: ${path} downloaded`, "success");
    return;
  }
  openModal(
    title,
    h("p", { class: "confirm-text mono saved-path" }, path),
    [
      { label: "Show in folder", alignLeft: true, run: async () => void (await api.revealFile(path)) },
      { label: "Close", primary: true, run: () => undefined },
    ],
    { width: 520 },
  );
}

// --- Import ------------------------------------------------------------------

interface ParsedFile {
  headers: string[];
  rows: Cell[][];
}

export function importDialog(target: TableTarget, columns: ColumnMeta[], onDone: () => void): void {
  const fileInput = h("input", { type: "file", accept: ".csv,.tsv,.txt,.json,.ndjson", class: "file-input" });
  const delimiter = h(
    "select",
    { class: "input" },
    ...[
      ["auto", "Detect"],
      [",", "Comma ,"],
      [";", "Semicolon ;"],
      ["\t", "Tab"],
      ["|", "Pipe |"],
    ].map(([value, label]) => h("option", { value }, label)),
  );
  const header = checkbox("First row is the header", true);
  const emptyAsNull = checkbox("Empty values become NULL", true);
  const mappingHost = h("div", { class: "import-mapping" });
  const preview = h("div", { class: "import-preview" });
  const status = h("p", { class: "test-status" }, "Choose a CSV or JSON file.");
  const insertable = columns.filter((column) => column.vectorDims == null);
  let text = "";
  let fileName = "";
  let parsed: ParsedFile | null = null;
  let mapping: string[] = [];

  const reparse = () => {
    if (!text) return;
    try {
      parsed = parseFile(text, fileName, delimiter.value, header.input.checked);
    } catch (error) {
      parsed = null;
      status.className = "test-status error";
      status.textContent = errorText(error);
      mappingHost.replaceChildren();
      preview.replaceChildren();
      return;
    }
    const byName = new Map(insertable.map((column) => [column.name.toLowerCase(), column.name]));
    mapping = parsed.headers.map((name) => byName.get(name.trim().toLowerCase()) ?? SKIP);
    renderMapping();
    status.className = "test-status";
    status.textContent = `${formatNumber(parsed.rows.length)} rows, ${parsed.headers.length} columns in ${fileName}.`;
  };

  const renderMapping = () => {
    if (!parsed) return;
    const current = parsed;
    const rows = current.headers.map((name, index) => {
      const select = h(
        "select",
        { class: "input mono" },
        h("option", { value: SKIP }, "(skip)"),
        ...insertable.map((column) => h("option", { value: column.name, selected: mapping[index] === column.name }, `${column.name}  ${column.typeName}`)),
      );
      if (mapping[index] === SKIP) select.value = SKIP;
      select.addEventListener("change", () => {
        mapping[index] = select.value;
        renderPreview();
      });
      const sample = current.rows.find((row) => row[index] !== null && row[index] !== "")?.[index] ?? "";
      return h(
        "tr",
        {},
        h("td", { class: "mono" }, name),
        h("td", {}, "→"),
        h("td", {}, select),
        h("td", { class: "import-sample", title: sample ?? "" }, sample ?? ""),
      );
    });
    mappingHost.replaceChildren(
      h(
        "table",
        { class: "import-table" },
        h("thead", {}, h("tr", {}, h("th", {}, "File column"), h("th", {}), h("th", {}, "Table column"), h("th", {}, "Sample"))),
        h("tbody", {}, ...rows),
      ),
    );
    renderPreview();
  };

  const renderPreview = () => {
    if (!parsed) return;
    const used = mapping.map((name, index) => ({ name, index })).filter((item) => item.name !== SKIP);
    if (used.length === 0) {
      preview.replaceChildren(h("p", { class: "field-hint" }, "Map at least one column."));
      return;
    }
    const head = h("tr", {}, ...used.map((item) => h("th", {}, item.name)));
    const body = parsed.rows
      .slice(0, PREVIEW_ROWS)
      .map((row) => h("tr", {}, ...used.map((item) => h("td", {}, cellText(normalize(row[item.index], emptyAsNull.input.checked))))));
    preview.replaceChildren(h("table", { class: "import-table compact" }, h("thead", {}, head), h("tbody", {}, ...body)));
  };

  fileInput.addEventListener("change", async () => {
    const file = fileInput.files?.[0];
    if (!file) return;
    fileName = file.name;
    text = await file.text();
    reparse();
  });
  delimiter.addEventListener("change", reparse);
  header.input.addEventListener("change", reparse);
  emptyAsNull.input.addEventListener("change", renderPreview);

  const info = driverInfo(target.driver);
  const body = h(
    "div",
    { class: "form-grid" },
    h(
      "p",
      { class: "form-intro" },
      "Rows are inserted into ",
      h("strong", {}, `${target.database}.${target.table}`),
      ` (${info.label}) in batches of ${IMPORT_BATCH}; each batch is one transaction, so a failed batch leaves the earlier ones in place.`,
    ),
    field("File", fileInput),
    h(
      "div",
      { class: "field-row even" },
      field("Delimiter (CSV)", delimiter),
      h("div", { class: "field import-options" }, header.element, emptyAsNull.element),
    ),
    mappingHost,
    preview,
    status,
  );
  openModal(
    `Import into ${target.table}`,
    body,
    [
      { label: "Cancel", run: () => undefined },
      {
        label: "Import",
        primary: true,
        run: async () => {
          if (!parsed) throw new Error("Choose a file first");
          const used = mapping.map((name, index) => ({ name, index })).filter((item) => item.name !== SKIP);
          if (used.length === 0) throw new Error("Map at least one column");
          const changes: RowChange[] = parsed.rows.map((row) => ({
            kind: "insert",
            values: used.map((item) => ({ column: item.name, value: normalize(row[item.index], emptyAsNull.input.checked) })),
          }));
          let done = 0;
          status.className = "test-status";
          // One import run is confirmed once (when the rules ask), for all of its batches.
          const confirmations = new ConfirmationSession();
          const importId = crypto.randomUUID();
          for (let start = 0; start < changes.length; start += IMPORT_BATCH) {
            const batch = changes.slice(start, start + IMPORT_BATCH);
            try {
              const affected = await confirmations.run((confirmation) =>
                api.applyChanges(target.connectionId, target.database, target.table, batch, "import", confirmation, importId),
              );
              if (affected === null) {
                status.textContent = `Import cancelled. ${formatNumber(done)} rows were imported before that.`;
                if (done > 0) onDone();
                return false;
              }
            } catch (error) {
              status.className = "test-status error";
              status.textContent = `Rows ${formatNumber(start + 1)}–${formatNumber(start + batch.length)} failed: ${errorText(error)}. ${formatNumber(done)} rows were imported before that.`;
              if (done > 0) onDone();
              return false;
            }
            done += batch.length;
            status.textContent = `Imported ${formatNumber(done)} / ${formatNumber(changes.length)} rows…`;
          }
          toast(`Imported ${formatNumber(done)} rows into ${target.table}`, "success");
          onDone();
        },
      },
    ],
    { width: 720 },
  );
}

function normalize(value: Cell, emptyAsNull: boolean): Cell {
  if (value === null) return null;
  return emptyAsNull && value === "" ? null : value;
}

function cellText(value: Cell): string {
  return value === null ? "NULL" : value.length > 40 ? `${value.slice(0, 39)}…` : value;
}

function parseFile(text: string, name: string, delimiter: string, hasHeader: boolean): ParsedFile {
  const trimmed = text.replace(LEADING_BOM, "").trim();
  if (/\.(json|ndjson)$/i.test(name) || trimmed.startsWith("[") || trimmed.startsWith("{")) return parseJson(trimmed);
  const separator = delimiter === "auto" ? detectDelimiter(trimmed) : delimiter;
  const records = parseCsv(trimmed, separator).filter((record) => record.some((value) => value !== ""));
  if (records.length === 0) throw new Error("The file has no rows");
  const width = Math.max(...records.map((record) => record.length));
  const headers = hasHeader
    ? records[0].map((value, index) => value.trim() || `column_${index + 1}`)
    : Array.from({ length: width }, (_, index) => `column_${index + 1}`);
  const rows = (hasHeader ? records.slice(1) : records).map((record) => headers.map((_, index) => record[index] ?? null));
  return { headers, rows };
}

function parseJson(text: string): ParsedFile {
  let items: unknown[];
  if (text.startsWith("[")) items = JSON.parse(text) as unknown[];
  else if (text.includes("\n") && !text.startsWith("{\n") && text.split("\n").every((line) => !line.trim() || line.trim().startsWith("{")))
    items = text
      .split("\n")
      .filter((line) => line.trim())
      .map((line) => JSON.parse(line) as unknown);
  else {
    const value = JSON.parse(text) as Record<string, unknown>;
    const list = Object.values(value).find(Array.isArray);
    items = list ?? [value];
  }
  const objects = items.filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null && !Array.isArray(item));
  if (objects.length === 0) throw new Error("Expected a JSON array of objects (or one object per line)");
  const headers: string[] = [];
  for (const object of objects) for (const key of Object.keys(object)) if (!headers.includes(key)) headers.push(key);
  const rows = objects.map((object) =>
    headers.map((key) => {
      const value = object[key];
      if (value === undefined || value === null) return null;
      return typeof value === "object" ? JSON.stringify(value) : String(value);
    }),
  );
  return { headers, rows };
}
