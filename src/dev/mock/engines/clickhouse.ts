/**
 * ClickHouse is planned: the mock has its descriptor only (shown in pickers with `localStorage["fosta.dev.showPlannedEngines"] = "1"`),
 * and connecting fails with `ENGINE_NOT_WIRED` like the backend. Its agent adds data and `tree` / `browse` / `run` here.
 */
import { DESCRIPTORS, ORDER } from "../descriptors";

export const descriptor = DESCRIPTORS.clickhouse;
export const order = ORDER.indexOf("clickhouse");
