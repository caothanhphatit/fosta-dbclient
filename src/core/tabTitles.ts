/**
 * Tab titles that tell tabs apart: two tabs titled "orders" from different connections show "orders · Staging" and
 * "orders · Production", and the database is added when it is what differs. Pure, so it is unit-tested.
 */

export interface TitledTab {
  title: string;
  /** The connection's display name; absent for tabs that belong to none (a command's log). */
  connection?: string;
  database?: string | null;
}

/** The title to show for each tab, in order: the plain title unless another tab has the same one. */
export function disambiguateTitles(tabs: TitledTab[]): string[] {
  const groups = new Map<string, TitledTab[]>();
  for (const tab of tabs) groups.set(tab.title, [...(groups.get(tab.title) ?? []), tab]);
  return tabs.map((tab) => {
    const group = groups.get(tab.title) ?? [];
    if (group.length < 2) return tab.title;
    const connections = new Set(group.map((item) => item.connection ?? ""));
    const sameConnection = group.filter((item) => (item.connection ?? "") === (tab.connection ?? ""));
    const databases = new Set(sameConnection.map((item) => item.database ?? ""));
    const parts = [tab.title];
    if (connections.size > 1 && tab.connection) parts.push(tab.connection);
    if (databases.size > 1 && tab.database) parts.push(tab.database);
    return parts.join(" · ");
  });
}

/** The tooltip of a tab: its connection, then the longer description (or the title). */
export function tabTooltip(tab: TitledTab & { subtitle?: string }): string {
  return [tab.connection, tab.subtitle ?? tab.title].filter(Boolean).join(" · ");
}
