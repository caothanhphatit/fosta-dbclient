/**
 * The server view: INFO (headline figures, keyspace, every section with a search box) and the SLOWLOG. A Redis Cluster
 * connection adds the cluster panel (state, slot coverage, shards; a node opens its own INFO), a Sentinel connection the
 * sentinel panel (master, replicas, sentinels), and a standalone connection to one node of a cluster a warning.
 */
import { formatNumber, h } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { button, cell, dataTable, emptyHint, labelled, textInput } from "./editorKit";
import {
  formatBytes,
  formatDuration,
  formatMicros,
  modeText,
  nodeHealth,
  slotCoverage,
  slotRanges,
  type ClusterNode,
  type ClusterOverview,
  type Health,
  type SentinelOverview,
  type SentinelPeer,
  type ServerInfo,
  type Slowlog,
} from "./model";
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
  private readonly banner = h("div", { class: "rd-banner rd-server-banner", hidden: true });
  private readonly overview = h("div", { class: "rd-cards" });
  private readonly topology = h("div", { hidden: true });
  private readonly nodeInfo = h("div", { class: "rd-node-info", hidden: true });
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
        this.banner,
        this.overview,
        this.topology,
        this.nodeInfo,
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
      this.drawBanner(info);
      this.drawOverview(info);
      this.drawTopology(info);
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
      card("Mode", modeText(info), info.node ? `INFO of ${info.node}` : ""),
      card("Uptime", info.uptimeSeconds === null ? "" : formatDuration(info.uptimeSeconds)),
      card("Clients", info.connectedClients === null ? "" : formatNumber(info.connectedClients)),
      card("Memory", memory, `peak ${formatBytes(info.usedMemoryPeak)}, policy ${info.maxmemoryPolicy ?? "unknown"}`),
      card("Commands", info.totalCommandsProcessed === null ? "" : formatNumber(info.totalCommandsProcessed)),
      card("Hit rate", hitRate(info.keyspaceHits, info.keyspaceMisses), "keyspace_hits / (hits + misses)"),
      card("Modules", info.modules.map((module) => module.name).join(", ") || "none"),
    );
  }

  private drawBanner(info: ServerInfo): void {
    this.banner.hidden = info.topology !== "clusterNode";
    this.banner.replaceChildren(
      h(
        "span",
        {},
        h("strong", {}, "This is one node of a cluster: "),
        "you see its keys only, and keys on other nodes answer MOVED. Edit the connection and switch Topology to Redis Cluster.",
      ),
    );
  }

  private drawTopology(info: ServerInfo): void {
    const parts = info.cluster ? this.clusterPanel(info.cluster) : info.sentinel ? this.sentinelPanel(info.sentinel) : [];
    this.topology.replaceChildren(...parts);
    this.topology.hidden = parts.length === 0;
    if (!info.cluster) this.nodeInfo.hidden = true;
  }

  private clusterPanel(cluster: ClusterOverview): HTMLElement[] {
    const summary = h(
      "div",
      { class: "rd-topology-summary" },
      badge(`state ${cluster.state || "unknown"}`, cluster.state === "ok" ? "ok" : "bad"),
      h("span", {}, slotCoverage(cluster)),
      cluster.knownNodes === null ? null : h("span", { class: "rd-note" }, `${formatNumber(cluster.knownNodes)} known nodes`),
    );
    const nodeButton = (node: ClusterNode) => {
      const health = node.health ?? "health unknown";
      const link = h(
        "button",
        { class: "link-button mono rd-node-link", type: "button", title: `${node.addr} (${node.role}, ${health}): show its INFO` },
        healthDot(nodeHealth(node.health), health),
        node.addr,
      );
      link.addEventListener("click", () => void this.showNode(node.addr));
      return link;
    };
    const table = dataTable(
      ["Master", "Replicas", "Slots", "Keys", "Memory"],
      cluster.shards.map((shard) =>
        h(
          "tr",
          {},
          cell(nodeButton(shard.master), shard.master.id ? h("div", { class: "rd-note mono", title: shard.master.id }, shard.master.id.slice(0, 12)) : null),
          cell(shard.replicas.length ? h("div", { class: "rd-node-list" }, ...shard.replicas.map(nodeButton)) : h("span", { class: "rd-note" }, "none")),
          cell(h("span", { class: "mono" }, slotRanges(shard.slots)), h("div", { class: "rd-note" }, `${formatNumber(shard.slotCount)} slots`)),
          cell(shard.keys === null ? "" : formatNumber(shard.keys)),
          cell(formatBytes(shard.usedMemory)),
        ),
      ),
    );
    return [h("div", { class: "section-title" }, "Cluster"), summary, table];
  }

  private sentinelPanel(sentinel: SentinelOverview): HTMLElement[] {
    const fact = (label: string, value: HTMLElement | string) => h("div", { class: "rd-fact" }, h("span", { class: "rd-card-label" }, label), value);
    const facts = h(
      "div",
      { class: "rd-topology-summary" },
      fact("Master name", h("strong", {}, sentinel.masterName)),
      fact("Current master", h("span", { class: "mono" }, sentinel.master)),
      fact("Flags", badge(sentinel.masterFlags || "unknown", nodeHealth(null, sentinel.masterFlags))),
      fact("Quorum", sentinel.quorum === null ? "?" : String(sentinel.quorum)),
    );
    const peers = (list: SentinelPeer[], withLink: boolean, empty: string) =>
      list.length === 0
        ? emptyHint(empty)
        : dataTable(
            withLink ? ["Address", "Flags", "Link"] : ["Address", "Flags"],
            list.map((peer) =>
              h(
                "tr",
                {},
                cell(healthDot(nodeHealth(peer.link, peer.flags), peer.flags), h("span", { class: "mono" }, peer.address)),
                cell(peer.flags),
                withLink ? cell(peer.link ?? "") : null,
              ),
            ),
          );
    return [
      h("div", { class: "section-title" }, "Sentinel"),
      facts,
      h("div", { class: "rd-subtitle" }, `Replicas (${sentinel.replicas.length})`),
      peers(sentinel.replicas, true, "The sentinels know no replica of this master."),
      h("div", { class: "rd-subtitle" }, `Sentinels (${sentinel.sentinels.length})`),
      peers(sentinel.sentinels, false, "No sentinel answered."),
    ];
  }

  /** One cluster node's INFO (`redis.info { node }`), under the cluster panel. */
  private async showNode(node: string): Promise<void> {
    const close = button("Close", () => (this.nodeInfo.hidden = true));
    const title = h("div", { class: "section-title rd-title-row" }, h("span", {}, `Node ${node}`), close);
    this.nodeInfo.replaceChildren(title, h("div", { class: "rd-note" }, "Reading INFO…"));
    this.nodeInfo.hidden = false;
    this.nodeInfo.scrollIntoView({ block: "nearest" });
    try {
      const info = await this.api.info(node);
      if (this.disposed) return;
      const facts = h(
        "div",
        { class: "rd-topology-summary" },
        h("strong", {}, `${info.flavor === "valkey" ? "Valkey" : "Redis"} ${info.version}`),
        h("span", {}, info.role === "slave" ? "replica" : info.role),
        info.uptimeSeconds === null ? null : h("span", {}, `up ${formatDuration(info.uptimeSeconds)}`),
        info.connectedClients === null ? null : h("span", {}, `${formatNumber(info.connectedClients)} clients`),
        h("span", {}, `${formatBytes(info.usedMemory)} used`),
      );
      this.nodeInfo.replaceChildren(title, facts, ...infoSections(info, ""));
    } catch (error) {
      if (this.disposed) return;
      this.nodeInfo.replaceChildren(title, emptyHint(`Could not read the INFO of ${node}.`));
      showError(error, `Read INFO of ${node}`);
    }
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
    const blocks = infoSections(info, this.search.value.trim().toLowerCase());
    this.sections.replaceChildren(...(blocks.length ? blocks : [emptyHint("No INFO field matches.")]));
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
    // A cluster's slow log merges every node.
    const withNode = log.entries.some((entry) => entry.node);
    this.slow.replaceChildren(
      header,
      dataTable(
        withNode ? ["Node", "Id", "When", "Took", "Command", "Client"] : ["Id", "When", "Took", "Command", "Client"],
        log.entries.map((entry) =>
          h(
            "tr",
            {},
            withNode ? cell(h("span", { class: "mono" }, entry.node ?? "")) : null,
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

/** A small state badge (cluster state, sentinel flags). */
function badge(text: string, health: Health): HTMLElement {
  return h("span", { class: `rd-badge ${health}` }, text);
}

function healthDot(health: Health, title: string): HTMLElement {
  return h("span", { class: `rd-dot ${health}`, title, "aria-label": title });
}

/** The INFO sections whose field or value contains `needle` (all when empty), each a collapsible table. */
function infoSections(info: ServerInfo, needle: string): HTMLElement[] {
  return Object.entries(info.sections)
    .map(([name, fields]) => ({
      name,
      rows: Object.entries(fields).filter(([key, value]) => !needle || key.toLowerCase().includes(needle) || value.toLowerCase().includes(needle)),
    }))
    .filter((block) => block.rows.length > 0)
    .map((block) => {
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
    });
}

export const serverViewFactory = (ctx: ViewContext): ServerView => new ServerView(ctx);
