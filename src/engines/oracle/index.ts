/**
 * Oracle UI module. Tables, views and materialized views use the shared grid and design view; this module adds the
 * definition view for PL/SQL source (packages, procedures, functions), sequences and synonyms, the icons the backend's
 * tree names as `engine:oracle/<name>`, and the Instant Client prompt shown when connecting fails because the client
 * library is missing (download from Oracle with progress and cancel, or a folder with an installed client).
 */
import type { EngineUiModule } from "../index";
import { isClientError } from "./clientApi";

const oracle: EngineUiModule = {
  id: "oracle",
  register: (register) =>
    register([
      {
        engine: "oracle",
        kinds: ["package", "procedure", "function", "sequence", "synonym"],
        role: "definition",
        icon: "design",
        title: (ctx) => `${ctx.object?.name ?? "Definition"} · source`,
        load: async () => (await import("./definitionView")).definitionView(),
      },
    ]),
  icons: {
    schema: "database",
    sequence: "expand",
    synonym: "bookmark",
    package: "layers",
  },
  connectError: async (error, context) => {
    if (!isClientError(error)) return false;
    const { promptForClient } = await import("./clientPrompt");
    return promptForClient(error, context);
  },
};

export default oracle;
