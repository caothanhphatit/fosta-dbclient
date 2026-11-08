// Builds the contract-test matrix for .github/workflows/live.yml from ci/engines/*.json.
//
//   node ci/plan.mjs --all                    every enabled engine (nightly, pushes to main, manual runs)
//   node ci/plan.mjs --changed <file>         engines whose `paths` match a changed file (one path per line)
//
// Prints `{"include":[{id, compose, version, versionEnv, seed, filter}, ...]}`, one entry per engine version. Entries with
// `"enabled": false` are skipped; `"nightlyOnly": true` entries only run with --all.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";

const root = join(import.meta.dirname, "engines");

/** `**` matches across folders, `*` within one folder. */
function globToRegExp(glob) {
  let pattern = "";
  for (let index = 0; index < glob.length; index += 1) {
    const char = glob[index];
    if (char === "*" && glob[index + 1] === "*") {
      pattern += ".*";
      index += 1;
    } else if (char === "*") {
      pattern += "[^/]*";
    } else {
      pattern += char.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
    }
  }
  return new RegExp(`^${pattern}$`);
}

export function plan(engines, changed) {
  const include = [];
  for (const engine of engines) {
    if (engine.enabled === false) continue;
    if (changed !== null) {
      if (engine.nightlyOnly) continue;
      const patterns = engine.paths.map(globToRegExp);
      if (!changed.some((file) => patterns.some((pattern) => pattern.test(file)))) continue;
    }
    for (const version of engine.versions) {
      include.push({
        id: engine.id,
        compose: engine.compose,
        version,
        versionEnv: engine.versionEnv,
        seed: engine.seed ?? "",
        filter: engine.filter,
      });
    }
  }
  return { include };
}

function main(args) {
  const engines = readdirSync(root)
    .filter((name) => name.endsWith(".json"))
    .sort()
    .map((name) => JSON.parse(readFileSync(join(root, name), "utf8")));
  let changed = null;
  if (args[0] === "--changed") {
    changed = readFileSync(args[1], "utf8")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
  } else if (args[0] !== "--all") {
    throw new Error("usage: node ci/plan.mjs --all | --changed <file>");
  }
  process.stdout.write(JSON.stringify(plan(engines, changed)));
}

main(process.argv.slice(2));
