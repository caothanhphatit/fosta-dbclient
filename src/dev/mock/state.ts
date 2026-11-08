/**
 * Config, workspaces and query history of the mock backend (this page load only). "Client A staging" is guarded (run an
 * UPDATE or DELETE to see the confirmation dialog) and has an unreachable server to see a failing connect; "Production"
 * is read-only. "Local" has a PostgreSQL profile: PostgreSQL is a planned engine, so connecting fails with ENGINE_NOT_WIRED.
 */
import { presetSettings } from "../../features/safety/safety";
import type { AppConfig, DriverId, HistoryEntry, Workspace } from "../../core/types";

const workspaces: Workspace[] = [
  {
    id: "local",
    name: "Local",
    color: "blue",
    icon: "laptop",
    safety: presetSettings("open"),
    connections: [
      {
        id: "c-local",
        name: "Local MariaDB",
        driver: "mariadb",
        host: "127.0.0.1",
        port: 3306,
        user: "root",
        database: "shop",
        savePassword: true,
        options: {},
      },
      {
        id: "c-local-mongo",
        name: "Local Mongo",
        driver: "mongodb",
        host: "127.0.0.1",
        port: 27017,
        user: "",
        database: "admin",
        savePassword: false,
        options: {},
      },
      {
        // A planned engine: connecting fails with ENGINE_NOT_WIRED, as in a real build.
        id: "c-local-pg",
        name: "Local PostgreSQL (planned)",
        // `DriverId` lists the wired engines only (widened in step 15); a saved profile can name any engine id.
        driver: "postgres" as DriverId,
        host: "127.0.0.1",
        port: 5432,
        user: "postgres",
        database: "postgres",
        savePassword: true,
        options: {},
      },
    ],
    tasks: [],
  },
  {
    id: "client-a",
    name: "Client A staging",
    color: "orange",
    icon: "cloud",
    // Guarded: run an UPDATE or DELETE in a query tab to see the confirmation dialog.
    safety: presetSettings("guarded"),
    connections: [
      {
        id: "c-a-1",
        name: "Staging (port-forward)",
        driver: "mariadb",
        host: "127.0.0.1",
        port: 13306,
        user: "app",
        database: "shop",
        savePassword: true,
        options: {},
      },
      {
        id: "c-a-2",
        name: "Analytics replica",
        driver: "mysql",
        host: "127.0.0.1",
        port: 13307,
        user: "readonly",
        database: null,
        savePassword: false,
        options: {},
      },
      {
        id: "c-a-3",
        name: "ERP (SQL Server)",
        driver: "mssql",
        host: "erp.client-a.local",
        port: 1433,
        user: "sa",
        database: "ERP",
        savePassword: true,
        options: { encrypt: "true" },
      },
      {
        id: "c-a-5",
        name: "Reporting replica (offline)",
        driver: "mariadb",
        host: "10.20.0.15",
        port: 3306,
        user: "report",
        database: "shop",
        savePassword: true,
        options: {},
      },
      {
        id: "c-a-4",
        name: "Events (Mongo Atlas)",
        driver: "mongodb",
        host: "cluster0.example.net",
        port: 27017,
        user: "app",
        database: "admin",
        savePassword: true,
        options: {},
      },
    ],
    tasks: [
      {
        id: "t-a-1",
        name: "MariaDB port-forward",
        program: "kubectl",
        args: ["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"],
        autoStart: true,
        autoRestart: true,
        readyPattern: "Forwarding from",
      },
    ],
  },
  {
    id: "prod",
    name: "Production",
    color: "red",
    icon: "shield",
    safety: presetSettings("readOnly"),
    connections: [
      {
        id: "c-prod",
        name: "Shop primary",
        driver: "mariadb",
        host: "db.prod.internal",
        port: 3306,
        user: "app",
        database: "shop",
        savePassword: true,
        options: {},
      },
    ],
    tasks: [],
  },
];

/** Mutable state of the mock backend for this page load. */
export const state: { config: AppConfig; history: HistoryEntry[]; historySequence: number } = {
  config: { workspaces, activeWorkspace: "client-a", embedding: null, aiConnections: [], aiActive: null, queryHistory: true },
  history: [],
  historySequence: 0,
};
