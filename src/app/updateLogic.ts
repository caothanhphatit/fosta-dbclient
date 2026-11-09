/** Pure parts of the update item (`update.ts`): labels, download progress and the web edition's commands. */

export const RELEASES_URL = "https://github.com/caothanhphatit/fosta-dbclient/releases";
export const DOCKER_IMAGE = "ghcr.io/caothanhphatit/fosta-studio";

/**
 * Where an update stands. `available`: found, not being installed here (portable, web, or the updater could not get it);
 * `downloading` / `ready`: the installed app fetches it in the background and installs it on quit.
 */
export type UpdatePhase = "none" | "available" | "downloading" | "ready";

export function releaseUrl(version: string): string {
  return `${RELEASES_URL}/tag/v${version}`;
}

/** The status bar item's text. */
export function updateLabel(phase: UpdatePhase, version: string, percent: number | null = null): string {
  switch (phase) {
    case "none":
      return "";
    case "ready":
      return "Restart to update";
    case "downloading":
      return percent === null ? `Update ${version}…` : `Update ${version} · ${percent}%`;
    case "available":
      return `Update ${version}`;
  }
}

/** Download progress as a whole percentage, or `null` while the size is unknown. */
export function downloadPercent(downloaded: number, total: number | null | undefined): number | null {
  if (!total || total <= 0) return null;
  return Math.max(0, Math.min(100, Math.floor((downloaded / total) * 100)));
}

/** Updating the web edition: pull, then recreate the container on the same data volume. */
export function dockerUpdateCommands(): string[] {
  return [
    `docker pull ${DOCKER_IMAGE}`,
    "docker rm -f fosta-studio",
    `docker run -d --name fosta-studio -p 127.0.0.1:8080:8080 -v fosta-studio-data:/data ${DOCKER_IMAGE}`,
  ];
}
