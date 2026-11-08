/** The server view: INFO (headline figures, keyspace, every section with a search box) and the SLOWLOG. */
import { formatNumber, h } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { button, cell, dataTable, emptyHint, labelled, textInput } from "./editorKit";
import { formatBytes, formatDuration, formatMicros, type ServerInfo, type Slowlog } from "./model";
import { RedisServerApi } from "./redisApi";
import "./redis.css";

const SLOWLOG_SIZES = [32, 128, 512];

/** Hit rate as a percentage text, or "" without any lookups. */
export function hitRate(hits: number | null, misses: number | null): string {
  const total = (hits ?? 0) + (misses ?? 0);
  return total === 0 ? "" : `${((100 * (hits ?? 0)) / total).toFixed(1)}%`;
}

/** The command of a slow log entry as one line, long arguments cut. */
export function commandLine(command: string[], limit = 200): string {
  const text = command.map((part) => (/[\s"']/.test(part) ? JSON.stringify(part) : part)).join(" ");
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

export class ServerView implements TabContent {
  readonly element: HTMLElement;
  private readonly api: RedisServerApi;
  private readonly overview = h("div", { class: "rd-cards" });
  private readonly keyspace = h("div", {});
  private readonly sections = h("div", { class: "rd-sections" });
  private readonly slow = h("div", {});
  private readonly search = textInput("", "Search INFO fields", "input rd-filter");
  private readonly size = h("select", { class: "input" }, ...SLOWLOG_SIZES.map((count) => h("option", { value: String(count) }, `last ${count}`)));
  private info: ServerInfo | null = null;
  private disposed = false;

  constructor(ctx: ViewContext) {
    this.api = new RedisServerApi(ctx.connection);
    this.size.value = "128";
    this.search.addEventListener("input", () => this.drawSections());
    this.size.addEventListener("change", () => void this.loadSlowlog());
    const toolbar = h(
      "div",
      { class: "toolbar rd-toolbar" },
      button("Refresh", () => void this.refresh(), "tool-button primary"),
      h("span", { class: "spacer" }),
      h("span", { class: "toolbar-title" }, ctx.profile.name),
    );
    this.element = h(
      "div",
      { class: "view rd-server" },
      toolbar,
      h(
        "div",
        { class: "rd-scroll rd-pad" },
        this.overview,
        h("div", { class: "section-title" }, "Keyspace"),
        this.keyspace,
        h(
          "div",
          { class: "section-title rd-title-row" },
          h("span", {}, "Slow log"),
          labelled("Show", this.size),
          button("Reload", () => void this.loadSlowlog()),
        ),
        this.slow,
        h("div", { class: "section-title rd-title-row" }, h("span", {}, "INFO"), this.search),
        this.sections,
      ),
    );
    void this.refresh();
  }

  dispose(): void {
    this.disposed = true;
  }

  async refresh(): Promise<void> {
    await Promise.all([this.loadInfo(), this.loadSlowlog()]);
  }

  private async loadInfo(): Promise<void> {
    try {
      const info = await this.api.info();
      if (this.disposed) return;
      this.info = info;
      this.drawOverview(info);
      this.drawKeyspace(info);
      this.drawSections();
    } catch (error) {
      if (!this.disposed) showError(error, "Read INFO");
    }
  }

  private drawOverview(info: ServerInfo): void {
    const card = (label: string, value: string, hint = "") =>
      h("div", { class: "rd-card", title: hint }, h("div", { class: "rd-card-label" }, label), h("div", { class: "rd-card-value" }, value));
    const memory = info.maxmemory ? `${formatBytes(info.usedMemory)} of ${formatBytes(info.maxmemory)}` : formatBytes(info.usedMemory);
    this.overview.replaceChildren(
      card("Version", `${info.flavor === "valkey" ? "Valkey" : "Redis"} ${info.version}`, info.os ?? ""),
      card("Mode", `${info.mode} · ${info.role === "slave" ? "replica" : info.role}`),
      card("Uptime", info.uptimeSeconds === null ? "" : formatDuration(info.uptimeSeconds)),
      card("Clients", info.connectedClients === null ? "" : formatNumber(info.connectedClients)),
      card("Memory", memory, `peak ${formatBytes(info.usedMemoryPeak)}, policy ${info.maxmemoryPolicy ?? "unknown"}`),
      card("Commands", info.totalCommandsProcessed === null ? "" : formatNumber(info.totalCommandsProcessed)),
      card("Hit rate", hitRate(info.keyspaceHits, info.keyspaceMisses), "keyspace_hits / (hits + misses)"),
      card("Modules", info.modules.map((module) => module.name).join(", ") || "none"),
    );
  }

  private drawKeyspace(info: ServerInfo): void {
    if (info.keyspace.length === 0) {
      this.keyspace.replaceChildren(emptyHint("No keys in any database."));
      return;
    }
    this.keyspace.replaceChildren(
      dataTable(
        ["Database", "Keys", "With a TTL", "Average TTL"],
        info.keyspace.map((db) =>
          h(
            "tr",
            {},
            cell(`db${db.db}${db.db === info.currentDb ? " (this connection)" : ""}`),
            cell(formatNumber(db.keys)),
            cell(formatNumber(db.expires)),
            cell(db.avgTtlMs ? `${formatDuration(Math.round(db.avgTtlMs / 1000))}` : ""),
          ),
        ),
      ),
    );
  }

  private drawSections(): void {
    const info = this.info;
    if (!info) return;
    const needle = this.search.value.trim().toLowerCase();
    const blocks = Object.entries(info.sections)
      .map(([name, fields]) => {
        const rows = Object.entries(fields).filter(([key, value]) => !needle || key.toLowerCase().includes(needle) || value.toLowerCase().includes(needle));
        return { name, rows };
      })
      .filter((block) => block.rows.length > 0);
    this.sections.replaceChildren(
      ...blocks.map((block) => {
        const details = h(
          "details",
          { class: "rd-section" },
          h("summary", {}, `${block.name} (${block.rows.length})`),
          dataTable(
            ["Field", "Value"],
            block.rows.map(([key, value]) => h("tr", {}, cell(h("span", { class: "mono" }, key)), cell(h("span", { class: "mono" }, value)))),
          ),
        );
        if (needle) details.open = true;
        return details;
      }),
    );
    if (blocks.length === 0) this.sections.replaceChildren(emptyHint("No INFO field matches."));
  }

  private async loadSlowlog(): Promise<void> {
    try {
      const log = await this.api.slowlog(Number(this.size.value) || 128);
      if (this.disposed) return;
      this.drawSlowlog(log);
    } catch (error) {
      if (this.disposed) return;
      this.slow.replaceChildren(emptyHint("The slow log is unavailable (ACL?)."));
      showError(error, "Read SLOWLOG");
    }
  }

  private drawSlowlog(log: Slowlog): void {
    const threshold = log.slowerThanUs === null ? "" : Number(log.slowerThanUs) < 0 ? "disabled" : formatMicros(Number(log.slowerThanUs));
    const header = h(
      "div",
      { class: "rd-note" },
      `${log.length === null ? "?" : formatNumber(log.length)} entries kept` +
        (threshold ? ` · logs commands slower than ${threshold}` : "") +
        (log.maxLen ? ` · keeps at most ${formatNumber(Number(log.maxLen))}` : ""),
    );
    if (log.entries.length === 0) {
      this.slow.replaceChildren(header, emptyHint("No slow commands recorded."));
      return;
    }
    this.slow.replaceChildren(
      header,
      dataTable(
        ["Id", "When", "Took", "Command", "Client"],
        log.entries.map((entry) =>
          h(
            "tr",
            {},
            cell(String(entry.id)),
            cell(new Date(entry.timestamp * 1000).toLocaleString()),
            cell(formatMicros(entry.durationUs)),
            cell(h("span", { class: "mono", title: entry.command.join(" ") }, commandLine(entry.command))),
            cell(entry.clientName ? `${entry.clientName} (${entry.client ?? ""})` : (entry.client ?? "")),
          ),
        ),
      ),
    );
  }
}

export const serverViewFactory = (ctx: ViewContext): ServerView => new ServerView(ctx);
