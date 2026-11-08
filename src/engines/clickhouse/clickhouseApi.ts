/**
 * Typed calls of the `clickhouse.*` operations over the injected `ConnectionApi` (`invokeCapability`). `kill` goes through
 * `withConfirmation`, so the workspace's safety rules apply: it resolves null when the user declined.
 */
import type { ConnectionApi } from "../../core/connectionApi";
import { withConfirmation } from "../../features/safety/confirmationDialog";
import type { OperationTable } from "./model";

export interface TableRef {
  database: string;
  table: string;
}

export class ClickHouseApi {
  constructor(private readonly connection: ConnectionApi) {}

  parts(ref: TableRef): Promise<OperationTable> {
    return this.connection.invokeCapability<OperationTable>("clickhouse.parts", ref);
  }

  mutations(ref: TableRef): Promise<OperationTable> {
    return this.connection.invokeCapability<OperationTable>("clickhouse.mutations", ref);
  }

  processes(): Promise<OperationTable> {
    return this.connection.invokeCapability<OperationTable>("clickhouse.processes", {});
  }

  queryLog(limit: number, failedOnly: boolean): Promise<OperationTable> {
    return this.connection.invokeCapability<OperationTable>("clickhouse.queryLog", { limit, failedOnly });
  }

  /** `KILL QUERY`; null when the confirmation was declined. */
  killQuery(queryId: string): Promise<unknown> {
    return withConfirmation((confirmation) => this.connection.invokeCapability("clickhouse.kill", { queryId }, confirmation));
  }

  /** `KILL MUTATION`; null when the confirmation was declined. */
  killMutation(ref: TableRef, mutationId: string): Promise<unknown> {
    return withConfirmation((confirmation) => this.connection.invokeCapability("clickhouse.kill", { ...ref, mutationId }, confirmation));
  }
}
