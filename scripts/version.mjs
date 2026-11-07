#!/usr/bin/env node
// Keeps the app version in step across every file that carries it.
//
//   node scripts/version.mjs get                    print the version (package.json is the source of truth)
//   node scripts/version.mjs check [X.Y.Z|vX.Y.Z]   fail when the files disagree, or differ from the given version
//   node scripts/version.mjs set <X.Y.Z|patch|minor|major>
//   node scripts/version.mjs changelog [X.Y.Z]      print that version's section of CHANGELOG.md (release notes)
//
// `set` rewrites package.json, the root entries of package-lock.json, src-tauri/tauri.conf.json,
// src-tauri/Cargo.toml and the app's entry in src-tauri/Cargo.lock. Edits are targeted text
// replacements so every file keeps its formatting. No dependencies.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const CRATE = "fosta-studio";

/** Version slots per file; each pattern captures [prefix, version]. The first slot is the source of truth. */
const targets = [
  { file: "package.json", patterns: [/^(\{[\s\S]*?\r?\n {2}"version":\s*)"([^"]*)"/] },
  {
    file: "package-lock.json",
    patterns: [/^(\{[\s\S]*?\r?\n {2}"version":\s*)"([^"]*)"/, /(\r?\n {4}"":\s*\{[\s\S]*?\r?\n {6}"version":\s*)"([^"]*)"/],
  },
  { file: "src-tauri/tauri.conf.json", patterns: [/^(\{[\s\S]*?\r?\n {2}"version":\s*)"([^"]*)"/] },
  { file: "src-tauri/Cargo.toml", patterns: [/(\[package\][^[]*?\r?\nversion\s*=\s*)"([^"]*)"/] },
  { file: "src-tauri/Cargo.lock", patterns: [new RegExp(`(\\[\\[package\\]\\]\\r?\\nname = "${CRATE}"\\r?\\nversion = )"([^"]*)"`)] },
];

function read(file) {
  return readFileSync(join(root, file), "utf8");
}

/** Returns [{ label, version }] for every slot, failing when a slot cannot be found. */
function readAll() {
  const found = [];
  for (const { file, patterns } of targets) {
    const text = read(file);
    patterns.forEach((pattern, index) => {
      const label = patterns.length > 1 ? `${file} (entry ${index + 1})` : file;
      const match = pattern.exec(text);
      if (!match) fail(`Could not find the version in ${label}.`);
      found.push({ label, version: match[2] });
    });
  }
  return found;
}

function current() {
  return readAll()[0].version;
}

function bump(version, part) {
  const match = SEMVER.exec(version);
  if (!match) fail(`The current version "${version}" is not a valid semantic version.`);
  let [major, minor, patch] = match.slice(1, 4).map(Number);
  if (part === "major") [major, minor, patch] = [major + 1, 0, 0];
  else if (part === "minor") [minor, patch] = [minor + 1, 0];
  else patch += 1;
  return `${major}.${minor}.${patch}`;
}

function normalize(value) {
  return value.replace(/^refs\/tags\//, "").replace(/^v/, "");
}

function set(arg) {
  if (!arg) fail("Usage: node scripts/version.mjs set <X.Y.Z|patch|minor|major>");
  const next = ["patch", "minor", "major"].includes(arg) ? bump(current(), arg) : normalize(arg);
  if (!SEMVER.test(next)) fail(`"${arg}" is not a semantic version (X.Y.Z) or one of patch, minor, major.`);
  readAll(); // make sure every slot exists before writing anything
  for (const { file, patterns } of targets) {
    let text = read(file);
    for (const pattern of patterns) text = text.replace(pattern, (_, prefix) => `${prefix}"${next}"`);
    writeFileSync(join(root, file), text);
  }
  console.log(`Version set to ${next}.`);
  console.log(`Next: move the Unreleased notes in CHANGELOG.md under [${next}], commit, then tag v${next}.`);
}

function check(expected) {
  const found = readAll();
  const want = expected ? normalize(expected) : found[0].version;
  if (!SEMVER.test(want)) fail(`"${want}" is not a valid semantic version.`);
  const wrong = found.filter((entry) => entry.version !== want);
  if (wrong.length > 0) {
    for (const entry of found) console.error(`  ${entry.version === want ? "ok      " : "MISMATCH"} ${entry.label}: ${entry.version}`);
    fail(`Version mismatch: expected ${want} everywhere. Fix it with "npm run version:set -- ${want}".`);
  }
  console.log(`Version ${want} is consistent across ${found.length} entries.`);
}

/** Body of the `## [X.Y.Z]` section of CHANGELOG.md, without its heading. */
function changelog(version) {
  const want = normalize(version ?? current());
  const lines = read("CHANGELOG.md").split(/\r?\n/);
  const start = lines.findIndex((line) => line.startsWith(`## [${want}]`));
  if (start < 0) fail(`CHANGELOG.md has no "## [${want}]" section. Move the Unreleased notes under a heading for ${want}.`);
  let end = lines.findIndex((line, index) => index > start && (line.startsWith("## ") || /^\[[^\]]+\]:\s/.test(line)));
  if (end < 0) end = lines.length;
  const body = lines
    .slice(start + 1, end)
    .join("\n")
    .trim();
  if (!body) fail(`The "## [${want}]" section of CHANGELOG.md is empty.`);
  console.log(body);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

const [command, arg] = process.argv.slice(2);
if (command === "set") set(arg);
else if (command === "check") check(arg);
else if (command === "changelog") changelog(arg);
else if (command === "get") console.log(current());
else fail("Usage: node scripts/version.mjs <get | check [X.Y.Z] | set <X.Y.Z|patch|minor|major> | changelog [X.Y.Z]>");
