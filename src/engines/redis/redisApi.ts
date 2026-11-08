/**
 * Typed calls of the `redis.*` operations over the injected `ConnectionApi` (`invokeCapability`). One instance serves one
 * logical database (`db`). Writes go through `withConfirmation`, so the workspace's safety rules apply: `null` from `edit`
 * means the user declined the confirmation.
 */
import type { ConnectionApi } from "../../core/connectionApi";
import { withConfirmation } from "../../features/safety/confirmationDialog";
import type { Blob, EditResult, Expect, KeyInfo, KeyPage, ReadOptions, RedisEdit, ServerInfo, Slowlog, ValueRead } from "./model";

export interface ScanOptions {
  cursor?: string;
  pattern?: string;
  typeFilter?: string;
  pageSize?: number;
  count?: number;
  withMemory?: boolean;
}

export class RedisApi {
  constructor(
    private readonly connection: ConnectionApi,
    readonly db: number,
  ) {}

  scan(options: ScanOptions = {}): Promise<KeyPage> {
    return this.connection.invokeCapability<KeyPage>("redis.scan", { db: this.db, ...options });
  }

  keyInfo(key: Blob): Promise<KeyInfo> {
    return this.connection.invokeCapability<KeyInfo>("redis.keyInfo", { db: this.db, key });
  }

  read(key: Blob, options: ReadOptions = {}): Promise<ValueRead> {
    return this.connection.invokeCapability<ValueRead>("redis.readValue", { db: this.db, key, ...options });
  }

  /**
   * Applies an edit. With `expect` it fails with `CONFLICT` (see `conflictOf`) when the value changed since it was read;
   * without, it overwrites. Resolves null when the confirmation was declined.
   */
  edit(edit: RedisEdit, expect?: Expect): Promise<EditResult | null> {
    const args = { db: this.db, ...edit, ...(expect ? { expect } : {}) };
    return withConfirmation((confirmation) => this.connection.invokeCapability<EditResult>("redis.edit", args, confirmation));
  }
}

/** The server-wide operations (no database). */
export class RedisServerApi {
  constructor(private readonly connection: ConnectionApi) {}

  info(): Promise<ServerInfo> {
    return this.connection.invokeCapability<ServerInfo>("redis.info", {});
  }

  slowlog(count = 128): Promise<Slowlog> {
    return this.connection.invokeCapability<Slowlog>("redis.slowlog", { count });
  }
}
