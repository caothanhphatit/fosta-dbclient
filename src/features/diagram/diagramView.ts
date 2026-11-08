/**
 * ER diagram of one database: tables with their columns and the foreign keys
 * between them, drawn as SVG. Auto layout ranks tables by their references
 * (parents left, children right); boxes can be dragged and their positions
 * are remembered per database. MongoDB has no foreign keys, so links are
 * inferred from field names such as userId → users (drawn dashed).
 */
import { showError } from "../../core/errorPanel";
import { api } from "../../core/api";
import type { ConnectionApi } from "../../core/connectionApi";
import type { EngineDescriptor, Relation, SchemaGraph, SchemaTable } from "../../core/contract";
import { showContextMenu } from "../../core/contextMenu";
import { copyText, formatCompact, h, icon, iconButton, iconOrDefault, separator, spacer, toolButton } from "../../core/dom";
import type { Navigator } from "../../core/navigator";
import { savedDialog } from "../importExport/importExport";
import type { TabContent } from "../../core/tabs";
import { onThemeChange } from "../../core/theme";
import type { DriverId } from "../../core/types";
import type { ViewContext } from "../../core/views";

const SVG = "http://www.w3.org/2000/svg";
const FONT = "12px 'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif";
const HEADER = 28;
const ROW = 20;
const PAD_X = 10;
const MIN_WIDTH = 170;
const MAX_WIDTH = 380;
const GAP_X = 110;
const GAP_Y = 36;
const STUB = 18;
const MIN_SCALE = 0.15;
/** Zoom used when jumping to a table (search, assistant), so it is readable. */
const FOCUS_SCALE = 0.9;
const MAX_SCALE = 2.5;
const STORAGE_PREFIX = "db-studio.diagram";
/** Below this zoom boxes show only their header; column text is unreadable anyway. */
const HEADER_ONLY_SCALE = 0.35;
/** Screen pixels around the viewport that still count as visible. */
const CULL_MARGIN = 200;

interface Box {
  table: SchemaTable;
  x: number;
  y: number;
  width: number;
  /** Columns shown in the box (all, or keys only). */
  rows: SchemaTable["columns"];
  group: SVGGElement | null;
  /** The column rows; hidden at low zoom. */
  body: SVGGElement | null;
}

interface Link {
  name: string;
  from: string;
  fromColumns: string[];
  to: string;
  toColumns: string[];
  inferred: boolean;
  path: SVGPathElement | null;
  marks: SVGGElement | null;
}

interface Palette {
  canvas: string;
  dot: string;
  surface: string;
  border: string;
  fg: string;
  fg2: string;
  fg3: string;
  accent: string;
  accentFg: string;
  view: string;
  collection: string;
  key: string;
  line: string;
  highlight: string;
}

export class DiagramView implements TabContent {
  readonly element: HTMLElement;
  private readonly connectionId: string;
  private readonly driver: DriverId;
  private readonly connection: Pick<ConnectionApi, "schemaGraph">;
  private readonly engine: EngineDescriptor;
  private readonly database: string;
  private readonly navigator: Navigator;
  private readonly host = h("div", { class: "diagram-host" });
  private readonly svg = document.createElementNS(SVG, "svg");
  private readonly viewport = document.createElementNS(SVG, "g");
  private readonly linkLayer = document.createElementNS(SVG, "g");
  private readonly boxLayer = document.createElementNS(SVG, "g");
  private readonly info = h("span", { class: "diagram-info" }, "Loading…");
  private readonly zoomLabel = h("span", { class: "diagram-zoom" }, "100%");
  private readonly search = h("input", { class: "input diagram-search", placeholder: "Find table…", spellcheck: "false" });
  private readonly measure = document.createElement("canvas").getContext("2d")!;
  private readonly unsubscribeTheme: () => void;
  private resizeObserver: ResizeObserver | null = null;
  private boxes = new Map<string, Box>();
  private links: Link[] = [];
  private palette: Palette = readPalette();
  private scale = 1;
  private tx = 40;
  private ty = 40;
  private selected: string | null = null;
  private showTypes = true;
  private keysOnly = false;
  private inferLinks: boolean;
  private graph: SchemaGraph | null = null;
  private fitted = false;
  private pendingFocus: string | null = null;
  private cullFrame: number | null = null;

  constructor(ctx: ViewContext, database: string) {
    this.connectionId = ctx.profile.id;
    this.driver = ctx.profile.driver;
    this.connection = ctx.connection;
    this.engine = ctx.engine;
    this.database = database;
    this.navigator = ctx.navigator;
    // Document stores declare no foreign keys; links guessed from field names are on by default there.
    this.inferLinks = ctx.engine.family === "document";
    this.measure.font = FONT;
    this.svg.classList.add("diagram-svg");
    this.viewport.append(this.linkLayer, this.boxLayer);
    this.svg.append(this.viewport);
    this.host.append(this.svg);

    const toggle = (label: string, checked: boolean, change: (value: boolean) => void, hidden = false) => {
      const input = h("input", { type: "checkbox", checked });
      input.addEventListener("change", () => change(input.checked));
      const element = h("label", { class: "diagram-toggle" }, input, label);
      element.hidden = hidden;
      return element;
    };
    this.search.addEventListener("input", () => this.applySearch(false));
    this.search.addEventListener("keydown", (event) => {
      if (event.key === "Enter") this.applySearch(true);
      if (event.key === "Escape") {
        this.search.value = "";
        this.applySearch(false);
      }
    });
    const toolbar = h(
      "div",
      { class: "toolbar" },
      icon(iconOrDefault(ctx.engine.icon, "database"), `driver-icon ${ctx.engine.id}`),
      h("span", { class: "toolbar-title" }, database),
      this.info,
      separator(),
      toolButton("refresh", "Reload", () => void this.load(), "Read the schema again"),
      toolButton("layers", "Auto layout", () => this.autoLayout(true), "Arrange tables by their references"),
      toolButton("fit", "Fit", () => this.fit(), "Fit the whole diagram in view"),
      iconButton("minus", "Zoom out", () => this.zoomBy(1 / 1.2)),
      this.zoomLabel,
      iconButton("plus", "Zoom in", () => this.zoomBy(1.2)),
      separator(),
      toggle("Types", true, (value) => {
        this.showTypes = value;
        this.rebuild();
      }),
      toggle("Keys only", false, (value) => {
        this.keysOnly = value;
        this.rebuild();
      }),
      toggle("Inferred links", this.inferLinks, (value) => {
        this.inferLinks = value;
        this.rebuild();
      }),
      spacer(),
      this.search,
      toolButton("download", "SVG", () => this.exportSvg(), "Save the diagram as SVG"),
      toolButton("download", "PNG", () => void this.exportPng(), "Save the diagram as PNG"),
    );
    this.element = h("div", { class: "view diagram-view" }, toolbar, this.host);
    this.installPanZoom();
    if (typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.scheduleCull());
      this.resizeObserver.observe(this.svg);
    }
    this.unsubscribeTheme = onThemeChange(() => {
      this.palette = readPalette();
      this.rebuild();
    });
    void this.load();
  }

  /** Selects and centres a table once the schema is loaded (used by the assistant). */
  focus(name: string): void {
    this.pendingFocus = name;
    this.applyPendingFocus();
  }

  private applyPendingFocus(): void {
    const box = this.pendingFocus
      ? (this.boxes.get(this.pendingFocus) ?? [...this.boxes.values()].find((item) => item.table.name.toLowerCase() === this.pendingFocus?.toLowerCase()))
      : undefined;
    if (!box || this.svg.getBoundingClientRect().width === 0) return;
    this.pendingFocus = null;
    this.fitted = true;
    this.scale = Math.max(this.scale, FOCUS_SCALE);
    this.select(box.table.name);
    this.centerOn(box);
  }

  onActivate(): void {
    if (!this.fitted && this.graph) requestAnimationFrame(() => this.fitIfNew());
  }

  dispose(): void {
    this.unsubscribeTheme();
    this.resizeObserver?.disconnect();
  }

  // --- Data -----------------------------------------------------------------

  private async load(): Promise<void> {
    this.info.textContent = "Reading schema…";
    try {
      this.graph = await this.connection.schemaGraph(this.database);
      this.rebuild();
      const { tables, totalTables } = this.graph;
      const capped = totalTables > tables.length ? ` (first ${tables.length} of ${totalTables})` : "";
      this.info.textContent = `${tables.length} ${this.engine.objectsLabel}${capped} · ${this.links.length} links`;
      requestAnimationFrame(() => {
        this.fitIfNew();
        this.applyPendingFocus();
      });
    } catch (error) {
      this.info.textContent = "";
      showError(error, "Load the diagram");
    }
  }

  /** Recomputes boxes and links from the graph and the current toggles, keeping positions. */
  private rebuild(): void {
    if (!this.graph) return;
    const previous = this.boxes;
    const stored = this.loadPositions();
    this.links = buildLinks(this.graph, this.inferLinks);
    const linked = new Set(this.links.flatMap((link) => [`${link.from}|${link.fromColumns.join(",")}`]));
    const fkColumns = new Map<string, Set<string>>();
    for (const link of this.links) {
      const set = fkColumns.get(link.from) ?? new Set<string>();
      link.fromColumns.forEach((column) => set.add(column));
      fkColumns.set(link.from, set);
    }
    void linked;
    this.boxes = new Map();
    for (const table of this.graph.tables) {
      const foreign = fkColumns.get(table.name) ?? new Set<string>();
      const rows = this.keysOnly ? table.columns.filter((column) => column.primary || foreign.has(column.name)) : table.columns;
      const old = previous.get(table.name) ?? stored[table.name];
      this.boxes.set(table.name, { table, x: old?.x ?? 0, y: old?.y ?? 0, width: this.boxWidth(table, rows), rows, group: null, body: null });
    }
    if (!this.graph.tables.every((table) => previous.has(table.name) || stored[table.name])) this.autoLayout(false);
    this.render();
  }

  // --- Layout ---------------------------------------------------------------

  private boxWidth(table: SchemaTable, rows: SchemaTable["columns"]): number {
    const text = (value: string) => this.measure.measureText(value).width;
    const header = text(table.name) + 70;
    const body = rows.reduce((widest, column) => Math.max(widest, text(column.name) + (this.showTypes ? text(column.columnType) + 34 : 0) + 30), 0);
    return Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, header, body)));
  }

  private boxHeight(box: Box): number {
    return HEADER + Math.max(1, box.rows.length) * ROW + 6;
  }

  /**
   * Layered layout: a table sits one column right of every table it references,
   * rows inside a column are ordered by the average position of their neighbours.
   * Tables without links are packed in a grid below.
   */
  private autoLayout(save: boolean): void {
    const names = [...this.boxes.keys()];
    const parents = new Map(names.map((name) => [name, new Set<string>()]));
    const neighbours = new Map(names.map((name) => [name, new Set<string>()]));
    for (const link of this.links) {
      if (link.from === link.to) continue;
      parents.get(link.from)?.add(link.to);
      neighbours.get(link.from)?.add(link.to);
      neighbours.get(link.to)?.add(link.from);
    }
    const rank = new Map<string, number>();
    const visiting = new Set<string>();
    const rankOf = (name: string): number => {
      const known = rank.get(name);
      if (known !== undefined) return known;
      if (visiting.has(name)) return 0;
      visiting.add(name);
      let value = 0;
      for (const parent of parents.get(name) ?? []) value = Math.max(value, rankOf(parent) + 1);
      visiting.delete(name);
      rank.set(name, value);
      return value;
    };
    const connected = names.filter((name) => (neighbours.get(name)?.size ?? 0) > 0);
    const isolated = names.filter((name) => (neighbours.get(name)?.size ?? 0) === 0).sort();
    const columns: string[][] = [];
    for (const name of connected.sort()) {
      const index = rankOf(name);
      (columns[index] ??= []).push(name);
    }
    const order = new Map<string, number>();
    const index = () => columns.forEach((column) => column.forEach((name, position) => order.set(name, position)));
    index();
    for (let pass = 0; pass < 4; pass += 1) {
      for (const column of columns) {
        if (!column) continue;
        const score = (name: string) => {
          const around = [...(neighbours.get(name) ?? [])].map((other) => order.get(other) ?? 0);
          return around.length ? around.reduce((sum, value) => sum + value, 0) / around.length : (order.get(name) ?? 0);
        };
        column.sort((a, b) => score(a) - score(b));
      }
      index();
    }
    let x = 0;
    let bottom = 0;
    const heights = columns.map((column) => (column ?? []).reduce((sum, name) => sum + this.boxHeight(this.boxes.get(name)!) + GAP_Y, -GAP_Y));
    const tallest = Math.max(0, ...heights);
    columns.forEach((column, columnIndex) => {
      if (!column) return;
      const width = Math.max(...column.map((name) => this.boxes.get(name)!.width));
      let y = (tallest - heights[columnIndex]) / 2;
      for (const name of column) {
        const box = this.boxes.get(name)!;
        box.x = x;
        box.y = y;
        y += this.boxHeight(box) + GAP_Y;
        bottom = Math.max(bottom, y);
      }
      x += width + GAP_X;
    });
    const gridWidth = Math.max(x - GAP_X, 1100);
    let gx = 0;
    let gy = connected.length ? bottom + GAP_Y * 2 : 0;
    let lineHeight = 0;
    for (const name of isolated) {
      const box = this.boxes.get(name)!;
      if (gx > 0 && gx + box.width > gridWidth) {
        gx = 0;
        gy += lineHeight + GAP_Y;
        lineHeight = 0;
      }
      box.x = gx;
      box.y = gy;
      gx += box.width + GAP_Y;
      lineHeight = Math.max(lineHeight, this.boxHeight(box));
    }
    if (save) {
      this.savePositions();
      this.render();
      this.fit();
    }
  }

  // --- Rendering ------------------------------------------------------------

  private render(): void {
    const palette = this.palette;
    this.svg.style.background = palette.canvas;
    this.svg.style.setProperty("--dot", palette.dot);
    this.boxLayer.replaceChildren();
    this.linkLayer.replaceChildren();
    for (const box of this.boxes.values()) {
      box.group = this.drawBox(box);
      this.boxLayer.append(box.group);
    }
    for (const link of this.links) {
      link.path = svg("path", { fill: "none", "stroke-width": "1.4" });
      link.marks = svg("g", { fill: "none", "stroke-width": "1.4" });
      const title = svg("title", {});
      title.textContent = `${link.inferred ? "Inferred: " : ""}${link.from}(${link.fromColumns.join(", ")}) → ${link.to}(${link.toColumns.join(", ")})${link.inferred ? "" : `\n${link.name}`}`;
      link.path.append(title);
      this.linkLayer.append(link.path, link.marks);
      this.routeLink(link);
    }
    this.applyTransform();
    this.applyHighlight();
    this.cull();
  }

  private scheduleCull(): void {
    if (this.cullFrame !== null) return;
    this.cullFrame = requestAnimationFrame(() => {
      this.cullFrame = null;
      this.cull();
    });
  }

  /**
   * Hides boxes and links outside the visible area, and column rows at low zoom,
   * so large schemas pan and zoom smoothly. Exports undo this on their clone.
   */
  private cull(): void {
    const rect = this.svg.getBoundingClientRect();
    // Not laid out yet (hidden tab): show everything rather than guess.
    const sized = rect.width > 0 && rect.height > 0;
    const margin = CULL_MARGIN / this.scale;
    const left = (0 - this.tx) / this.scale - margin;
    const top = (0 - this.ty) / this.scale - margin;
    const right = (rect.width - this.tx) / this.scale + margin;
    const bottom = (rect.height - this.ty) / this.scale + margin;
    const overlaps = (x1: number, y1: number, x2: number, y2: number) => !sized || (x2 >= left && x1 <= right && y2 >= top && y1 <= bottom);
    const headerOnly = sized && this.scale < HEADER_ONLY_SCALE;
    for (const box of this.boxes.values()) {
      const visible = overlaps(box.x, box.y, box.x + box.width, box.y + this.boxHeight(box));
      setDisplayed(box.group, visible);
      setDisplayed(box.body, !headerOnly);
    }
    for (const link of this.links) {
      const from = this.boxes.get(link.from);
      const to = this.boxes.get(link.to);
      if (!from || !to) continue;
      // Routes stay within the two boxes' span plus the stubs on either side.
      const x1 = Math.min(from.x, to.x) - STUB * 3;
      const x2 = Math.max(from.x + from.width, to.x + to.width) + STUB * 3;
      const y1 = Math.min(from.y, to.y);
      const y2 = Math.max(from.y + this.boxHeight(from), to.y + this.boxHeight(to)) + 14;
      const visible = overlaps(x1, y1, x2, y2);
      setDisplayed(link.path, visible);
      setDisplayed(link.marks, visible);
    }
  }

  private drawBox(box: Box): SVGGElement {
    const palette = this.palette;
    const { table } = box;
    const height = this.boxHeight(box);
    const color = table.kind === "view" ? palette.view : this.engine.family === "document" ? palette.collection : palette.accent;
    const group = svg("g", { class: "diagram-box", transform: `translate(${box.x},${box.y})` });
    group.dataset.table = table.name;
    group.append(
      svg("rect", { class: "diagram-frame", width: box.width, height, rx: 7, fill: palette.surface, stroke: color, "stroke-width": "1.2" }),
      svg("path", { d: `M0 7a7 7 0 0 1 7-7h${box.width - 14}a7 7 0 0 1 7 7v${HEADER - 7}H0z`, fill: color }),
      text(table.name, PAD_X, 18, { fill: palette.accentFg, "font-weight": "600" }),
    );
    const meta = [table.kind === "view" ? "view" : "", table.rows !== null ? `~${formatCompact(table.rows)}` : ""].filter(Boolean).join(" · ");
    if (meta) group.append(text(meta, box.width - PAD_X, 18, { fill: palette.accentFg, "text-anchor": "end", opacity: "0.8", "font-size": "11" }));
    if (box.rows.length === 0) {
      group.append(text(table.columns.length ? "no key columns" : "columns unavailable", PAD_X, HEADER + 14, { fill: palette.fg3, "font-style": "italic" }));
    }
    const foreign = new Set(this.links.filter((link) => link.from === table.name).flatMap((link) => link.fromColumns));
    const body = svg("g", { class: "diagram-body" });
    box.body = body;
    group.append(body);
    box.rows.forEach((column, index) => {
      const y = HEADER + index * ROW + 14;
      const isForeign = foreign.has(column.name);
      if (column.primary) body.append(svg("path", { d: `M${PAD_X + 4} ${y - 8}l4 4-4 4-4-4z`, fill: palette.key }));
      else if (isForeign) body.append(svg("path", { d: `M${PAD_X + 4} ${y - 8}l4 4-4 4-4-4z`, fill: "none", stroke: palette.key, "stroke-width": "1.2" }));
      const name = text(column.name, PAD_X + 14, y, { fill: column.primary ? palette.fg : palette.fg2, "font-weight": column.primary ? "600" : "400" });
      const tip = svg("title", {});
      tip.textContent = `${column.name} ${column.columnType} ${column.nullable ? "NULL" : "NOT NULL"}${column.primary ? " · primary key" : ""}${isForeign ? " · foreign key" : ""}`;
      name.append(tip);
      body.append(name);
      if (this.showTypes)
        body.append(text(truncate(column.columnType, 28), box.width - PAD_X, y, { fill: palette.fg3, "text-anchor": "end", "font-size": "11" }));
    });
    group.addEventListener("dblclick", () => this.openData(table));
    group.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      showContextMenu(event, [
        { label: "Open data", icon: "table", run: () => this.openData(table) },
        { label: "Select related", icon: "diagram", run: () => this.select(table.name) },
        { label: "Copy name", icon: "copy", run: () => void copyText(table.name) },
      ]);
    });
    return group;
  }

  /** Orthogonal route from the FK column on the child to the key column on the parent. */
  private routeLink(link: Link): void {
    const from = this.boxes.get(link.from);
    const to = this.boxes.get(link.to);
    if (!from || !to || !link.path || !link.marks) return;
    const rowY = (box: Box, column: string) => {
      const index = box.rows.findIndex((item) => item.name === column);
      return box.y + (index < 0 ? HEADER / 2 : HEADER + index * ROW + ROW / 2);
    };
    const y1 = rowY(from, link.fromColumns[0]);
    const y2 = rowY(to, link.toColumns[0]);
    let x1: number;
    let x2: number;
    let d1: number;
    let d2: number;
    if (link.from === link.to) {
      x1 = x2 = from.x + from.width;
      d1 = d2 = 1;
    } else if (from.x + from.width + STUB * 2 < to.x) {
      [x1, d1, x2, d2] = [from.x + from.width, 1, to.x, -1];
    } else if (to.x + to.width + STUB * 2 < from.x) {
      [x1, d1, x2, d2] = [from.x, -1, to.x + to.width, 1];
    } else {
      // Overlapping columns: leave and enter on the same side, outside both boxes.
      const right = Math.max(from.x + from.width, to.x + to.width);
      [x1, d1, x2, d2] = [from.x + from.width, 1, to.x + to.width, 1];
      const out = right + STUB * 2;
      link.path.setAttribute("d", `M${x1} ${y1}H${out}V${y2}H${x2}`);
      this.drawEnds(link, x1, y1, d1, x2, y2, d2);
      return;
    }
    const a = x1 + d1 * STUB;
    const b = x2 + d2 * STUB;
    const middle = link.from === link.to ? a + STUB : (a + b) / 2;
    const path = link.from === link.to ? `M${x1} ${y1}H${middle}V${y2 + (y2 === y1 ? 14 : 0)}H${x2}` : `M${x1} ${y1}H${a}H${middle}V${y2}H${b}H${x2}`;
    link.path.setAttribute("d", path);
    this.drawEnds(link, x1, y1, d1, x2, y2, d2);
  }

  /** Crow's foot (many, with a ring when the FK is nullable) on the child, a bar (one) on the parent. */
  private drawEnds(link: Link, x1: number, y1: number, d1: number, x2: number, y2: number, d2: number): void {
    const marks = link.marks!;
    marks.replaceChildren();
    const child = this.boxes.get(link.from)!.table;
    const optional = link.fromColumns.some((name) => child.columns.find((column) => column.name === name)?.nullable);
    const foot = 10;
    marks.append(
      svg("path", { d: `M${x1 + d1 * foot} ${y1}L${x1} ${y1 - 6}M${x1 + d1 * foot} ${y1}L${x1} ${y1 + 6}M${x1 + d1 * foot} ${y1}H${x1}` }),
      svg("path", { d: `M${x2 + d2 * 7} ${y2 - 6}V${y2 + 6}` }),
    );
    if (optional) marks.append(svg("circle", { cx: x1 + d1 * (foot + 5), cy: y1, r: 3.5, fill: this.palette.canvas }));
  }

  private applyHighlight(): void {
    const palette = this.palette;
    const related = new Set<string>();
    if (this.selected) {
      related.add(this.selected);
      for (const link of this.links) {
        if (link.from === this.selected) related.add(link.to);
        if (link.to === this.selected) related.add(link.from);
      }
    }
    for (const box of this.boxes.values()) {
      box.group?.classList.toggle("dimmed", this.selected !== null && !related.has(box.table.name));
      box.group?.classList.toggle("selected", box.table.name === this.selected);
    }
    for (const link of this.links) {
      const active = this.selected !== null && (link.from === this.selected || link.to === this.selected);
      const color = active ? palette.highlight : palette.line;
      for (const element of [link.path, link.marks]) {
        element?.setAttribute("stroke", color);
        element?.setAttribute("stroke-width", active ? "2" : "1.4");
        element?.setAttribute("opacity", this.selected !== null && !active ? "0.25" : "1");
      }
      link.path?.setAttribute("stroke-dasharray", link.inferred ? "5 4" : "");
    }
  }

  private select(name: string | null): void {
    this.selected = name;
    this.applyHighlight();
  }

  private applySearch(jump: boolean): void {
    const term = this.search.value.trim().toLowerCase();
    let first: Box | null = null;
    for (const box of this.boxes.values()) {
      const match = term !== "" && box.table.name.toLowerCase().includes(term);
      box.group?.classList.toggle("match", match);
      if (match && !first) first = box;
    }
    if (jump && first) {
      // Zoom in to a readable size (as the assistant's "show table" does), so the label and the view change together.
      this.scale = Math.max(this.scale, FOCUS_SCALE);
      this.select(first.table.name);
      this.centerOn(first);
    }
  }

  // --- Pan, zoom, drag ------------------------------------------------------

  private installPanZoom(): void {
    this.svg.addEventListener(
      "wheel",
      (event) => {
        event.preventDefault();
        const rect = this.svg.getBoundingClientRect();
        this.zoomAt(event.deltaY < 0 ? 1.1 : 1 / 1.1, event.clientX - rect.left, event.clientY - rect.top);
      },
      { passive: false },
    );
    this.svg.addEventListener("mousedown", (start) => {
      if (start.button !== 0) return;
      const target = (start.target as Element).closest<SVGGElement>(".diagram-box");
      const box = target ? this.boxes.get(target.dataset.table ?? "") : undefined;
      const origin = { x: start.clientX, y: start.clientY };
      const startPosition = box ? { x: box.x, y: box.y } : { x: this.tx, y: this.ty };
      let moved = false;
      const move = (event: MouseEvent) => {
        const dx = event.clientX - origin.x;
        const dy = event.clientY - origin.y;
        if (!moved && Math.abs(dx) + Math.abs(dy) < 3) return;
        moved = true;
        if (box) {
          box.x = Math.round(startPosition.x + dx / this.scale);
          box.y = Math.round(startPosition.y + dy / this.scale);
          box.group?.setAttribute("transform", `translate(${box.x},${box.y})`);
          for (const link of this.links) if (link.from === box.table.name || link.to === box.table.name) this.routeLink(link);
        } else {
          this.tx = startPosition.x + dx;
          this.ty = startPosition.y + dy;
          this.applyTransform();
        }
      };
      const release = () => {
        document.removeEventListener("mousemove", move);
        document.removeEventListener("mouseup", release);
        this.svg.classList.remove("panning");
        if (box && moved) {
          this.savePositions();
          this.scheduleCull();
        }
        if (!moved) this.select(box ? box.table.name : null);
      };
      if (!box) this.svg.classList.add("panning");
      document.addEventListener("mousemove", move);
      document.addEventListener("mouseup", release);
    });
  }

  private zoomAt(factor: number, cx: number, cy: number): void {
    const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, this.scale * factor));
    this.tx = cx - ((cx - this.tx) * next) / this.scale;
    this.ty = cy - ((cy - this.ty) * next) / this.scale;
    this.scale = next;
    this.applyTransform();
  }

  private zoomBy(factor: number): void {
    const rect = this.svg.getBoundingClientRect();
    this.zoomAt(factor, rect.width / 2, rect.height / 2);
  }

  private applyTransform(): void {
    this.viewport.setAttribute("transform", `translate(${this.tx},${this.ty}) scale(${this.scale})`);
    this.zoomLabel.textContent = `${Math.round(this.scale * 100)}%`;
    this.svg.style.backgroundSize = `${20 * this.scale}px ${20 * this.scale}px`;
    this.svg.style.backgroundPosition = `${this.tx}px ${this.ty}px`;
    this.scheduleCull();
  }

  private bounds(): { x: number; y: number; width: number; height: number } {
    const boxes = [...this.boxes.values()];
    if (!boxes.length) return { x: 0, y: 0, width: 1, height: 1 };
    const left = Math.min(...boxes.map((box) => box.x));
    const top = Math.min(...boxes.map((box) => box.y));
    const right = Math.max(...boxes.map((box) => box.x + box.width)) + STUB * 3;
    const bottom = Math.max(...boxes.map((box) => box.y + this.boxHeight(box)));
    return { x: left, y: top, width: right - left, height: bottom - top };
  }

  private fit(): void {
    const rect = this.svg.getBoundingClientRect();
    if (rect.width === 0) return;
    const area = this.bounds();
    this.scale = Math.min(1, Math.max(MIN_SCALE, Math.min((rect.width - 60) / area.width, (rect.height - 60) / area.height)));
    this.tx = (rect.width - area.width * this.scale) / 2 - area.x * this.scale;
    this.ty = (rect.height - area.height * this.scale) / 2 - area.y * this.scale;
    this.applyTransform();
  }

  private fitIfNew(): void {
    if (this.fitted || this.svg.getBoundingClientRect().width === 0) return;
    this.fitted = true;
    this.fit();
  }

  private centerOn(box: Box): void {
    const rect = this.svg.getBoundingClientRect();
    this.tx = rect.width / 2 - (box.x + box.width / 2) * this.scale;
    this.ty = rect.height / 2 - (box.y + this.boxHeight(box) / 2) * this.scale;
    this.applyTransform();
  }

  // --- Persistence, export, navigation --------------------------------------

  private storageKey(): string {
    return `${STORAGE_PREFIX}.${this.connectionId}.${this.database}`;
  }

  private loadPositions(): Record<string, { x: number; y: number }> {
    try {
      return JSON.parse(localStorage.getItem(this.storageKey()) ?? "{}") as Record<string, { x: number; y: number }>;
    } catch {
      return {};
    }
  }

  private savePositions(): void {
    const positions = Object.fromEntries([...this.boxes.values()].map((box) => [box.table.name, { x: box.x, y: box.y }]));
    try {
      localStorage.setItem(this.storageKey(), JSON.stringify(positions));
    } catch {
      // Storage unavailable: positions last for this tab only.
    }
  }

  /** A standalone SVG of the whole diagram (colours are already resolved, so it renders anywhere). */
  private serialize(): { markup: string; width: number; height: number } {
    const area = this.bounds();
    const margin = 24;
    const width = Math.ceil(area.width + margin * 2);
    const height = Math.ceil(area.height + margin * 2);
    const clone = this.viewport.cloneNode(true) as SVGGElement;
    clone.setAttribute("transform", `translate(${margin - area.x},${margin - area.y})`);
    clone.querySelectorAll(".dimmed").forEach((element) => element.classList.remove("dimmed"));
    // Viewport culling hides parts of the live diagram; the export has everything.
    clone.querySelectorAll<SVGElement>("[style]").forEach((element) => {
      element.style.removeProperty("display");
      if (!element.getAttribute("style")) element.removeAttribute("style");
    });
    const markup =
      `<svg xmlns="${SVG}" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" font-family="Segoe UI, system-ui, sans-serif" font-size="12">` +
      `<rect width="100%" height="100%" fill="${this.palette.canvas}"/>${new XMLSerializer().serializeToString(clone)}</svg>`;
    return { markup, width, height };
  }

  private exportSvg(): void {
    void saveFile(`${this.database}-diagram.svg`, this.serialize().markup, null);
  }

  private async exportPng(): Promise<void> {
    const { markup, width, height } = this.serialize();
    const image = new Image();
    image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(markup)}`;
    await image.decode();
    const ratio = 2;
    const canvas = document.createElement("canvas");
    canvas.width = width * ratio;
    canvas.height = height * ratio;
    const context = canvas.getContext("2d")!;
    context.scale(ratio, ratio);
    context.drawImage(image, 0, 0);
    const base64 = canvas.toDataURL("image/png").split(",")[1];
    await saveFile(`${this.database}-diagram.png`, null, base64);
  }

  private openData(table: SchemaTable): void {
    this.navigator.openTable({
      connectionId: this.connectionId,
      driver: this.driver,
      database: this.database,
      table: table.name,
      kind: table.kind === "view" ? "view" : "table",
    });
  }
}

/** Foreign keys from the server, plus (optionally) links guessed from `xxxId` / `xxx_id` field names. */
function buildLinks(graph: SchemaGraph, infer: boolean): Link[] {
  const links: Link[] = graph.relations.map((relation: Relation) => ({
    name: relation.constraint,
    from: relation.table,
    fromColumns: relation.columns,
    to: relation.referencedTable,
    toColumns: relation.referencedColumns,
    inferred: false,
    path: null,
    marks: null,
  }));
  if (!infer) return links;
  const byName = new Map(graph.tables.map((table) => [table.name.toLowerCase(), table]));
  const declared = new Set(links.map((link) => `${link.from}|${link.fromColumns[0]}`));
  for (const table of graph.tables) {
    for (const column of table.columns) {
      if (column.primary || declared.has(`${table.name}|${column.name}`)) continue;
      const match = /^(.+?)(?:_id|Id|ID)$/.exec(column.name);
      if (!match) continue;
      const base = match[1].replace(/_$/, "").toLowerCase();
      const candidates = [base, `${base}s`, `${base}es`, base.replace(/y$/, "ies")];
      const target = candidates.map((name) => byName.get(name)).find((item) => item && item.name !== table.name);
      const key = target?.columns.find((item) => item.primary);
      if (!target || !key) continue;
      links.push({ name: "", from: table.name, fromColumns: [column.name], to: target.name, toColumns: [key.name], inferred: true, path: null, marks: null });
    }
  }
  return links;
}

function svg<K extends keyof SVGElementTagNameMap>(tag: K, attributes: Record<string, string | number>): SVGElementTagNameMap[K] {
  const element = document.createElementNS(SVG, tag);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, String(value));
  return element;
}

function text(value: string, x: number, y: number, attributes: Record<string, string | number>): SVGTextElement {
  const element = svg("text", { x, y, ...attributes });
  element.textContent = value;
  return element;
}

function setDisplayed(element: SVGElement | null, visible: boolean): void {
  if (!element) return;
  const value = visible ? "" : "none";
  if (element.style.display !== value) element.style.display = value;
}

function truncate(value: string, limit: number): string {
  return value.length > limit ? `${value.slice(0, limit - 1)}…` : value;
}

/** Resolves the theme's CSS variables into concrete colours, so exports look like the screen. */
function readPalette(): Palette {
  const style = getComputedStyle(document.documentElement);
  const value = (name: string) => style.getPropertyValue(name).trim();
  return {
    canvas: value("--canvas"),
    dot: value("--border-strong"),
    surface: value("--surface"),
    border: value("--border-strong"),
    fg: value("--fg"),
    fg2: value("--fg-2"),
    fg3: value("--fg-3"),
    accent: value("--accent"),
    accentFg: value("--accent-fg"),
    view: value("--ws-purple"),
    collection: value("--ws-green"),
    key: value("--warning"),
    line: value("--fg-3"),
    highlight: value("--accent"),
  };
}

async function saveFile(name: string, text: string | null, base64: string | null): Promise<void> {
  try {
    savedDialog("Diagram saved", await api.saveDownload(name, text, base64));
  } catch (error) {
    showError(error, "Save the diagram");
  }
}
