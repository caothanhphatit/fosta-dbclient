/**
 * The store one engine workspace tab shares between its sections: the Documents section's filter read by the
 * Aggregations section, the fields the Schema section sampled read by the query bar's completion, and so on. Values
 * live as long as the tab; what should come back after a restart goes through each section's `saveState` instead.
 */

type Listener = (value: unknown) => void;

export class WorkspaceStore {
  private readonly values = new Map<string, unknown>();
  private readonly listeners = new Map<string, Set<Listener>>();

  get<T>(key: string): T | undefined {
    return this.values.get(key) as T | undefined;
  }

  /** Sets `key` and tells its subscribers (not when the value is the same one). */
  set<T>(key: string, value: T): void {
    if (this.values.has(key) && Object.is(this.values.get(key), value)) return;
    this.values.set(key, value);
    for (const listener of [...(this.listeners.get(key) ?? [])]) listener(value);
  }

  /** Calls `listener` whenever `key` changes; returns the function that stops it. */
  subscribe<T>(key: string, listener: (value: T) => void): () => void {
    let set = this.listeners.get(key);
    if (!set) {
      set = new Set();
      this.listeners.set(key, set);
    }
    const wrapped = listener as Listener;
    set.add(wrapped);
    return () => void this.listeners.get(key)?.delete(wrapped);
  }

  /** Drops every value and subscriber (the tab closed). */
  clear(): void {
    this.values.clear();
    this.listeners.clear();
  }
}
