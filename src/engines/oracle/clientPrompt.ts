/**
 * "Oracle Instant Client is required": shown when connecting fails because the client library is missing or cannot be
 * loaded. It offers the pinned download (from Oracle, under Oracle's license, with a progress bar and cancel), a folder
 * with a client that is already installed (saved as the profile's "Instant Client folder"), or cancel. Resolves true
 * when the connect should be retried.
 *
 * Without a pinned package for the platform (e.g. macOS) it explains how to install the client instead.
 */
import { capabilities } from "../../core/capabilities";
import type { AppError } from "../../core/contract";
import { field, openModal, textInput, type ModalAction } from "../../core/dialogs";
import { h } from "../../core/dom";
import { errorText, toAppError } from "../../core/errors";
import type { ConnectErrorContext } from "../../core/views";
import { clientApi, clientErrorKind, INSTANT_CLIENT_URL, LICENSE_URL, megabytes, progressText, type ClientStatus, type DownloadOffer } from "./clientApi";
import "./oracle.css";

const CLIENT_DIR = "clientDir";

function link(href: string, text: string): HTMLAnchorElement {
  return h("a", { href, target: "_blank", rel: "noopener noreferrer" }, text);
}

function offerOf(error: AppError, status: ClientStatus | null): DownloadOffer | null {
  if (status?.download) return status.download;
  const offer = (error as AppError & { download?: DownloadOffer }).download;
  return offer && typeof offer.size === "number" ? offer : null;
}

/** What to do without a download for this platform. */
function instructions(web: boolean): HTMLElement {
  return h(
    "ol",
    { class: "oracle-client-steps" },
    h("li", {}, "Download Instant Client Basic or Basic Light (19c or newer) for this system from ", link(INSTANT_CLIENT_URL, "Oracle"), "."),
    h(
      "li",
      {},
      web
        ? "Unpack it on the server (Docker: into /data) and enter that folder below, or add it to LD_LIBRARY_PATH and restart."
        : "Unpack it and enter that folder below, or add it to PATH and restart Fosta Studio.",
    ),
    h("li", {}, "On Linux the client also needs libaio (libaio1 or libaio1t64)."),
  );
}

export async function promptForClient(error: AppError, context: ConnectErrorContext): Promise<boolean> {
  const status = await clientApi.status().catch(() => null);
  const offer = offerOf(error, status);
  const web = capabilities().edition === "web";
  const kind = clientErrorKind(error);
  const where = web ? "on the Fosta Studio server" : "on this computer";

  const message = h(
    "p",
    {},
    kind === "load"
      ? `An Oracle Instant Client was found but could not be loaded: ${error.message}`
      : `Connecting to Oracle needs Oracle Instant Client ${where}${offer ? ` (${megabytes(offer.size)})` : ""}.`,
  );
  const license = offer
    ? h(
        "p",
        { class: "oracle-client-license" },
        `Downloads Instant Client Basic Light ${offer.version} from Oracle, verifies its checksum and installs it in the data folder. Oracle's software, under the `,
        link(offer.licenseUrl || LICENSE_URL, "Oracle Instant Client license"),
        ".",
        web ? " Linux servers also need libaio." : "",
      )
    : instructions(web);
  const bar = h("progress", { class: "oracle-client-progress", max: 1, value: 0 });
  const progressLine = h("div", { class: "oracle-client-status" });
  const progress = h("div", { class: "oracle-client-download", hidden: true }, bar, progressLine);
  const feedback = h("p", { class: "oracle-client-error", hidden: true });
  const folder = textInput(context.profile.options?.[CLIENT_DIR] ?? "", {
    placeholder: web ? "/data/oracle/instantclient_23_26" : "C:\\oracle\\instantclient_23_26",
  });
  const folderField = context.saveProfile
    ? field("Instant Client folder", folder, `Or use a client that is already installed ${where}: the folder with ${web ? "libclntsh.so" : "oci.dll"}.`)
    : h("p", { class: "field-hint" }, "Or set Advanced → Instant Client folder.");

  const showError = (text: string) => {
    feedback.hidden = false;
    feedback.textContent = text;
  };

  return new Promise<boolean>((resolve) => {
    let retry = false;
    let downloading = false;
    const actions: ModalAction[] = [
      {
        label: "Cancel",
        alignLeft: true,
        run: async () => {
          if (!downloading) return;
          await clientApi.cancel();
          return false;
        },
      },
    ];
    if (context.saveProfile) {
      const save = context.saveProfile;
      actions.push({
        label: "Use folder",
        primary: !offer,
        run: async () => {
          const dir = folder.value.trim();
          if (!dir) {
            showError("Enter the folder that holds Oracle Instant Client.");
            folder.focus();
            return false;
          }
          if (downloading) return false;
          await save({ ...context.profile, options: { ...context.profile.options, [CLIENT_DIR]: dir } });
          retry = true;
        },
      });
    }
    if (offer) {
      actions.push({
        label: `Download (${megabytes(offer.size)})`,
        primary: true,
        run: async () => {
          if (downloading) return false;
          downloading = true;
          feedback.hidden = true;
          progress.hidden = false;
          bar.value = 0;
          progressLine.textContent = "Starting the download…";
          try {
            await clientApi.download((event) => {
              bar.value = event.total > 0 ? event.downloaded / event.total : 0;
              progressLine.textContent = progressText(event);
            });
            retry = true;
            return;
          } catch (failure) {
            const app = toAppError(failure);
            progress.hidden = true;
            showError(clientErrorKind(app) === "cancelled" ? "The download was cancelled. Download again to resume it." : errorText(app));
            return false;
          } finally {
            downloading = false;
          }
        },
      });
    }
    openModal(
      kind === "load" ? "Oracle Instant Client could not be loaded" : "Oracle Instant Client is required",
      h("div", { class: "oracle-client-prompt" }, message, license, progress, folderField, feedback),
      actions,
      {
        width: 520,
        onClose: () => {
          if (downloading) void clientApi.cancel();
          resolve(retry);
        },
      },
    );
  });
}
