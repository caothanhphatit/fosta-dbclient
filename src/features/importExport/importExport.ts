/**
 * Table import (CSV, TSV, JSON, NDJSON) and export (CSV, CSV for Excel, JSON, SQL INSERT, Markdown) for the data tab.
 * Export can cover every row matching the current filter, read page by page with `browse_object` (by keyset where the
 * engine offers one, so a large export stays linear). Files are written to Downloads by the backend. Imports parse each
 * value for its column's kind first (nothing is sent while a value does not fit), then insert through `apply_edits`
 * (caller "import", one `importId` per run, so the safety rules confirm the run once) in batches; each batch is applied
 * as one set.
 */
import { capabilities } from "../../core/capabilities";
import { api, errorCode, errorText, type ExportRequest } from "../../core/api";
import type { ConnectionApi } from "../../core/connectionApi";
import type { BrowseRequest, ColumnInfo, ObjectRef, Page, Total, Value } from "../../core/contract";
import { checkbox, field, openModal, textInput, toast } from "../../core/dialogs";
import { formatNumber, h } from "../../core/dom";
import type { SqlDialect } from "../../core/sql";
import type { Cell } from "../../core/types";
import { isEditableColumn } from "../grid/changeSet";
import { detectDelimiter, parseCsv } from "./csv";
import { excelSeparator, exportDelimited, exportRows } from "./exporters";
import { importChanges, importValue } from "./importValues";
import { ExportCancelled, exportFileName, fetchAllRows, type PageCursor } from "./exportFetch";
import { ConfirmationSession } from "../safety/confirmationDialog";

const IMPORT_BATCH = 500;
const PREVIEW_ROWS = 6;
const SKIP = "__skip__";
/** Byte order mark: Excel needs it to read UTF-8; imports drop it. */
const BOM = String.fromCharCode(0xfeff);
const LEADING_BOM = new RegExp(`^${BOM}`);

type FileFormat = "csv" | "excel" | "json" | "insert" | "markdown";

const FORMATS: { value: FileFormat; label: string; extension: string }[] = [
  { value: "csv", label: "CSV", extension: "csv" },
  { value: "excel", label: `CSV for Excel (UTF-8 with BOM, "${excelSeparator()}" separated, from your locale)`, extension: "csv" },
  { value: "json", label: "JSON", extension: "json" },
  { value: "insert", label: "SQL INSERT statements", extension: "sql" },
  { value: "markdown", label: "Markdown table", extension: "md" },
];

export interface ExportSource {
  /** The object's name: the dialog title, the default file name and the INSERT target. */
  title: string;
  columns: ColumnInfo[];
  /** Rows of the page on screen. */
  pageRows: Value[][];
  /** Whether a filter is applied (the "all rows" choice says so). */
  filtered: boolean;
  total: Total | null;
  dialect: SqlDialect;
  /** Reads one page of every row matching the current filter and sort. */
  read: (cursor: PageCursor, limit: number) => Promise<Page>;
  /** The same rows for the backend: CSV and JSON of every row are streamed to the file instead of built in the page. */
  stream?: { connectionId: string; request: BrowseRequest };
}

/** Formats the backend writes itself, byte for byte as `exporters.ts` would (see `src-tauri/src/export.rs`). */
const STREAMED: FileFormat[] = ["csv", "excel", "json"];

/**
 * Every row through the backend, which reads the pages and writes them as they come: desktop into Downloads (with
 * progress; closing the dialog stops it and removes the partial file), web as the browser's download of a streamed
 * response. Memory stays at one page either way.
 */
async function streamExport(
  stream: NonNullable<ExportSource["stream"]>,
  format: FileFormat,
  fileName: string,
  signal: AbortSignal,
  onProgress: (rows: number) => void,
): Promise<void> {
  const request: ExportRequest = {
    browse: stream.request,
    format: format as ExportRequest["format"],
    separator: format === "excel" ? excelSeparator() : null,
    fileName,
    requestId: `export-${crypto.randomUUID()}`,
  };
  if (capabilities().browserDownloads) {
    const ticket = await api.exportPrepare(stream.connectionId, request);
    const link = h("a", { href: `/api/export/${encodeURIComponent(ticket)}`, download: fileName, hidden: true });
    document.body.append(link);
    link.click();
    link.remove();
    toast(`${fileName}: the download has started`, "success");
    return;
  }
  const stop = () => void api.cancelRequest(request.requestId ?? "").catch(() => undefined);
  signal.addEventListener("abort", stop);
  try {
    const done = await api.exportRows(stream.connectionId, request, (progress) => onProgress(progress.rows));
    if (!signal.aborted) savedDialog(`Exported ${formatNumber(done.rows)} rows`, done.path);
  } catch (error) {
    if (signal.aborted || errorCode(error) === "CANCELLED") return;
    throw error;
  } finally {
    signal.removeEventListener("abort", stop);
  }
}

export function exportDialog(source: ExportSource): void {
  const format = h("select", { class: "input" }, ...FORMATS.map((item) => h("option", { value: item.value }, item.label)));
  const scopeAll = h("input", { type: "radio", name: "scope", checked: true });
  const scopePage = h("input", { type: "radio", name: "scope" });
  const fileName = textInput(`${source.title}.csv`, { class: "input mono" });
  const progress = h("p", { class: "test-status" });
  format.addEventListener("change", () => {
    const extension = FORMATS.find((item) => item.value === format.value)?.extension ?? "txt";
    fileName.value = fileName.value.replace(/\.[^.]*$/, "") + `.${extension}`;
  });
  const filtered = source.filtered ? " matching the filter" : "";
  const totalText = source.total ? `${source.total.exact ? "" : "about "}${formatNumber(source.total.value)} ` : "";
  const body = h(
    "div",
    { class: "form-grid" },
    field("Format", format),
    h(
      "div",
      { class: "field" },
      h("span", { class: "field-label" }, "Rows"),
      h("label", { class: "checkbox" }, scopeAll, h("span", {}, `All ${totalText}rows${filtered}`)),
      h("label", { class: "checkbox" }, scopePage, h("span", {}, `This page only (${formatNumber(source.pageRows.length)} rows)`)),
    ),
    field("File name", fileName, capabilities().browserDownloads ? "Downloaded by your browser" : "Saved to your Downloads folder"),
    progress,
  );
  // Closing the dialog (Esc, Cancel) while rows are being read stops the export.
  const cancel = new AbortController();
  const of = source.total ? ` / ${source.total.exact ? "" : "~"}${formatNumber(source.total.value)}` : "";
  openModal(
    `Export ${source.title}`,
    body,
    [
      { label: "Cancel", run: () => undefined },
      {
        label: "Export",
        primary: true,
        run: async () => {
          try {
            const chosenFormat = FORMATS.find((item) => item.value === format.value) ?? FORMATS[0];
            if (!scopePage.checked && source.stream && STREAMED.includes(chosenFormat.value)) {
              const name = exportFileName(fileName.value, source.title, chosenFormat.extension);
              await streamExport(
                source.stream,
                chosenFormat.value,
                name,
                cancel.signal,
                (done) => (progress.textContent = `Writing rows… ${formatNumber(done)}${of}`),
              );
              return;
            }
            const rows = scopePage.checked
              ? source.pageRows
              : await fetchAllRows(source.read, cancel.signal, (done) => (progress.textContent = `Reading rows… ${formatNumber(done)}${of}`));
            if (cancel.signal.aborted) return;
            progress.textContent = "Writing file…";
            const chosen = FORMATS.find((item) => item.value === format.value) ?? FORMATS[0];
            const text = render(chosen.value, source.columns, rows, source.title, source.dialect);
            const path = await api.saveDownload(exportFileName(fileName.value, source.title, chosen.extension), text, null);
            savedDialog(`Exported ${formatNumber(rows.length)} rows`, path);
          } catch (error) {
            if (error instanceof ExportCancelled) return;
            throw error;
          }
        },
      },
    ],
    { width: 480, onClose: () => cancel.abort() },
  );
}

function render(format: FileFormat, columns: ColumnInfo[], rows: Value[][], table: string, dialect: SqlDialect): string {
  switch (format) {
    case "excel":
      // Excel reads UTF-8 only with a BOM, and splits on the locale's list separator ("," in the US, ";" in Vietnam or Germany).
      return `${BOM}${exportDelimited(columns, rows, excelSeparator())}`;
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

export interface ImportTarget {
  title: string;
  /** "shop.products (MariaDB)", for the intro line. */
  where: string;
  object: ObjectRef;
  columns: ColumnInfo[];
  connection: Pick<ConnectionApi, "applyEdits">;
  onDone: () => void;
}

export function importDialog(target: ImportTarget): void {
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
  const insertable = target.columns.filter(isEditableColumn);
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

  const used = () =>
    mapping.flatMap((name, index) => {
      const column = insertable.find((item) => item.name === name);
      return column ? [{ index, column }] : [];
    });

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
    const columns = used();
    if (columns.length === 0) {
      preview.replaceChildren(h("p", { class: "field-hint" }, "Map at least one column."));
      return;
    }
    const head = h("tr", {}, ...columns.map((item) => h("th", {}, item.column.name)));
    const body = parsed.rows.slice(0, PREVIEW_ROWS).map((row) =>
      h(
        "tr",
        {},
        ...columns.map((item) => {
          const value = importValue(item.column, row[item.index] ?? null, emptyAsNull.input.checked);
          return value instanceof Error
            ? h("td", { class: "import-invalid", title: value.message }, cellText(row[item.index] ?? null))
            : h("td", { class: value.t === "null" ? "null" : "" }, value.t === "null" ? "NULL" : cellText(row[item.index] ?? null));
        }),
      ),
    );
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

  const body = h(
    "div",
    { class: "form-grid" },
    h(
      "p",
      { class: "form-intro" },
      "Rows are inserted into ",
      h("strong", {}, target.where),
      ` in batches of ${IMPORT_BATCH}; a failed batch leaves the earlier ones in place.`,
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
    `Import into ${target.title}`,
    body,
    [
      { label: "Cancel", run: () => undefined },
      {
        label: "Import",
        primary: true,
        run: async () => {
          if (!parsed) throw new Error("Choose a file first");
          const columns = used();
          if (columns.length === 0) throw new Error("Map at least one column");
          const { changes, errors, errorCount } = importChanges(parsed.rows, columns, emptyAsNull.input.checked);
          if (errorCount > 0) {
            status.className = "test-status error";
            const more = errorCount > errors.length ? ` …and ${formatNumber(errorCount - errors.length)} more.` : "";
            status.textContent = `${formatNumber(errorCount)} value(s) do not fit their column; nothing was imported. ${errors.join(" · ")}${more}`;
            return false;
          }
          let done = 0;
          status.className = "test-status";
          // One import run is confirmed once (when the rules ask), for all of its batches.
          const confirmations = new ConfirmationSession();
          const importId = crypto.randomUUID();
          for (let start = 0; start < changes.length; start += IMPORT_BATCH) {
            const batch = changes.slice(start, start + IMPORT_BATCH);
            try {
              const result = await confirmations.run((confirmation) =>
                target.connection.applyEdits({ object: target.object, changes: batch }, "import", confirmation, importId),
              );
              if (result === null) {
                status.textContent = `Import cancelled. ${formatNumber(done)} rows were imported before that.`;
                if (done > 0) target.onDone();
                return false;
              }
            } catch (error) {
              status.className = "test-status error";
              status.textContent = `Rows ${formatNumber(start + 1)}–${formatNumber(start + batch.length)} failed: ${errorText(error)}. ${formatNumber(done)} rows were imported before that.`;
              if (done > 0) target.onDone();
              return false;
            }
            done += batch.length;
            status.textContent = `Imported ${formatNumber(done)} / ${formatNumber(changes.length)} rows…`;
          }
          toast(`Imported ${formatNumber(done)} rows into ${target.title}`, "success");
          target.onDone();
        },
      },
    ],
    { width: 720 },
  );
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
