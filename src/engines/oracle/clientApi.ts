/**
 * Oracle Instant Client provisioning as the backend's `oracle_client_*` commands see it (`runtime/oracle_client.rs`):
 * status, a download that streams progress, and cancel. Errors carry the code `ORACLE_CLIENT` and a `kind`.
 */
import type { AppError } from "../../core/contract";
import { invoke, invokeStreaming } from "../../core/transport";

export const CLIENT_ERROR = "ORACLE_CLIENT";

/** Oracle's download page and the license the downloaded client is covered by. */
export const INSTANT_CLIENT_URL = "https://www.oracle.com/database/technologies/instant-client/downloads.html";
export const LICENSE_URL = "https://www.oracle.com/downloads/licenses/instant-client-lic.html";

/** `ClientErrorKind` in `instant_client.rs`. */
export type ClientErrorKind = "notInstalled" | "unsupportedPlatform" | "network" | "checksumMismatch" | "diskFull" | "cancelled" | "archive" | "io" | "load";

export interface DownloadOffer {
  version: string;
  /** Bytes of the archive. */
  size: number;
  url: string;
  licenseUrl: string;
}

export interface ClientStatus {
  available: boolean;
  source: "system" | "downloaded" | "none";
  version: string | null;
  path: string | null;
  loaded: boolean;
  /** Absent when no package is pinned for this OS / CPU (then the user installs the client). */
  download: DownloadOffer | null;
}

export interface Progress {
  phase: "downloading" | "verifying" | "extracting" | "done";
  downloaded: number;
  total: number;
}

export const clientApi = {
  status: () => invoke<ClientStatus>("oracle_client_status"),
  /** Downloads, verifies, installs and loads the pinned client; resolves with the new status. */
  download: (onProgress: (event: Progress) => void) => invokeStreaming<ClientStatus, Progress>("oracle_client_download", {}, "onEvent", onProgress),
  cancel: () => invoke<void>("oracle_client_cancel"),
};

/** Whether a connect error means the client is missing or could not be loaded. */
export function isClientError(error: AppError): boolean {
  return error.code === CLIENT_ERROR || error.message.startsWith("Oracle Instant Client was not found");
}

/** The error's `kind` (`notInstalled` when it has none). */
export function clientErrorKind(error: AppError): ClientErrorKind {
  const kind = (error as AppError & { kind?: unknown }).kind;
  return typeof kind === "string" ? (kind as ClientErrorKind) : "notInstalled";
}

/** "60 MB" for a byte count (Oracle's sizes are about tens of megabytes). */
export function megabytes(bytes: number): string {
  return `${Math.max(1, Math.round(bytes / 1_000_000))} MB`;
}

/** The progress line under the bar. */
export function progressText(event: Progress): string {
  switch (event.phase) {
    case "downloading":
      return `Downloading ${megabytes(event.downloaded)} of ${megabytes(event.total)}`;
    case "verifying":
      return "Checking the download against Oracle's checksum…";
    case "extracting":
      return "Unpacking…";
    case "done":
      return "Installed";
  }
}
