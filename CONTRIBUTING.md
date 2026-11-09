# Contributing to Fosta Studio

Thanks for helping. This guide covers the local setup, the checks CI runs, and the conventions of the code base.
[ARCHITECTURE.md](ARCHITECTURE.md) explains how the pieces fit together.

> **License and CLA**: Fosta Studio is licensed under the [GPL v3.0](LICENSE). On your first pull request a bot asks you to sign the
> [Contributor License Agreement](CLA.md) by posting a comment; you keep the copyright in your work, and the CLA lets the
> maintainer also offer Fosta Studio under commercial terms.

Questions? Ask on [Discord](https://discord.gg/pBFkZD5mhN); use GitHub Issues for bugs and feature requests.

## Project layout

```
src/                     TypeScript UI (no framework, bundled by Vite)
  main.ts                app shell: layout, wiring, current config
  app/                   tab navigation, welcome screen, assistant context
  core/                  shared building blocks: IPC client (api.ts), types, DOM helpers, dialogs, tabs, theme, SQL helpers
  features/<area>/       one folder per feature (connections, workspaces, tasks, grid, table, query, design,
                         diagram, vector, assistant, importExport), each with its own dialogs
  dev/mock/              in-browser mock backend for `npm run dev` (one module per engine)
  styles.css             all styles
src-tauri/src/           Rust backend
  lib.rs                 the shared backend: AppState, Capabilities, module list
  main.rs                desktop front-end: Tauri setup and the command wrappers
  server/                web front-end (feature `web`): HTTP API, sign-in, static UI
  commands/              command bodies, grouped by area; for_each_command! lists them
  contract/              engine contract: Engine / Connection / Session traits, typed values, DTOs, contract suites
  runtime/               engine registry, ConnectionManager, crash containment
  gateway/               statement gateway: classification, safety rules, confirmations
  ai/                    assistant: prompt, tools, Anthropic and OpenAI wire formats
  config.rs secrets.rs   fosta-studio.json, Credential Manager / encrypted secret file
  tasks.rs job.rs        workspace commands and their child processes (connection_command.rs: a connection's command)
  engines/               engine adapters; engines/mysql is MariaDB/MySQL with its vector tools, engines/mssql SQL Server,
                         engines/mongo MongoDB
dev/                     docker-compose.yml and seed data for the test databases
scripts/                 toolchain installer and icon generator (PowerShell), version.mjs (version bump and check)
deploy/                  docker-compose.yml for the web edition (Dockerfile at the root)
```

## Toolchain

- **Node 20+** (CI uses Node 22) and npm.
- **Rust stable**, 1.88 or newer, with `rustfmt` and `clippy` (`rustup component add rustfmt clippy`).
- A Windows linker, one of:
  - **MSVC** (recommended for release builds): Visual Studio Build Tools with the C++ x64 workload and a Windows SDK. The script below
    installs Build Tools and Rust under `D:\tools` (admin rights needed once):

    ```powershell
    powershell -ExecutionPolicy Bypass -File scripts\setup-toolchain.ps1
    ```

  - **GNU**: the `stable-x86_64-pc-windows-gnu` toolchain (`rustup default stable-x86_64-pc-windows-gnu`) plus MinGW's `dlltool.exe` on
    `PATH`, e.g. from MSYS2 (`pacman -S mingw-w64-x86_64-binutils`). In Git Bash, prefix Rust commands with the MinGW path:

    ```sh
    export PATH="/c/msys64/mingw64/bin:$PATH"
    cd src-tauri && cargo check
    ```

## Running

```powershell
npm install
npm run tauri dev      # desktop app with hot reload
npm run dev            # UI only, in a browser, against the mock backend (src/dev/mock/)
npm run tauri build -- --no-bundle   # portable src-tauri\target\release\fosta-studio.exe
```

The mock backend is loaded only when Vite runs in dev mode and the page is not inside Tauri, so it never reaches the desktop build. Keep it
roughly in step with the real commands when you change their shapes.

## Checks

CI (`.github/workflows/ci.yml`, Windows) runs all of these; run them before opening a pull request:

```sh
npm run version:check   # package.json, tauri.conf.json and Cargo.toml carry the same version
npx tsc                 # type check
npm run lint            # ESLint (typescript-eslint recommended rules)
npm test                # Vitest unit tests (src/**/*.test.ts)
npm run build           # production bundle

cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test              # unit tests; live tests are #[ignore]d

# The web server (CI builds it on Linux; it also builds on Windows)
cargo clippy --all-targets --no-default-features --features web -- -D warnings
cargo test --no-default-features --features web
```

## Test databases and contract tests

`dev/compose/<engine>.yml` starts one engine each (`dev/docker-compose.yml` includes them all): MariaDB 11.8 (`shop`, with a vector
column), SQL Server 2022 (`ERP`), MongoDB 8 (`events`) and the others, all with generated sample data. The credentials in those files
are for local testing only.

```sh
docker compose -f dev/compose/mariadb.yml up -d --wait
sh dev/seed-mssql.sh    # after starting SQL Server: it has no init directory; this loads dev/seed/mssql.sql once
cd src-tauri
cargo test contract_mariadb -- --ignored --test-threads=1
```

Each engine's `engines/<id>/contract_tests.rs` runs the shared contract suites plus its own `contract_<id>_*` checks; CI runs them per
engine from `ci/engines/<id>.json` (`.github/workflows/live.yml`). A few machine-specific tests are named `live_*`:

- `live_saved_passwords_are_readable` expects saved passwords in Credential Manager for connections with the ids `local-mariadb`,
  `local-mssql` and `local-mongo` (create them in the app with "Save password" ticked, or skip this test).
- `live_assistant_tools` talks to the Fosta API and needs `FOSTA_KEY` in the environment; without it the test prints a note and passes.
- `live_save_download` writes into your Downloads folder and cleans up.
- The Redis and Elasticsearch adapters have `live_*` tests against their containers.

## Code style

- **Formatting**: `cargo fmt` with `src-tauri/rustfmt.toml` (140 columns). Prettier for TypeScript and CSS (`.prettierrc.json`: double
  quotes, semicolons, 2 spaces, 160 columns, trailing commas): `npm run format` / `npm run format:check`. `.editorconfig` covers editors.
- **TypeScript**: strict mode, no framework. Build DOM with `h()` from `core/dom.ts`; talk to the backend only through `core/api.ts`.
  Feature code goes under `src/features/<area>/`, dialogs next to the feature that opens them. Keep heavy dependencies out of the main
  chunk: the query view (CodeMirror) and the dev mock are loaded with dynamic `import()`.
- **Rust**: errors are `AppError`s shown to the user, so write them as sentences that say what to do (structured ones, such as the gateway's `CONFIRMATION_REQUIRED`, carry a JSON payload with a `code` and a `message`). Serialised types use
  `#[serde(rename_all = "camelCase")]` and must match `src/core/types.ts`.
- **Compatibility**: Tauri command names and argument names, the JSON shapes, the `fosta-studio.json` layout, the keyring service
  `fosta-studio` and its account names, and the `db-studio.*` localStorage keys are a contract with existing installs. Change them only with
  a migration.
- Unit-test pure logic (parsers, SQL builders) next to the code: `foo.test.ts` with Vitest, `#[cfg(test)] mod tests` in Rust.

## Adding an engine

The checklist is in [ARCHITECTURE.md](ARCHITECTURE.md#how-to-add-an-engine). In short: write the engine's `descriptor.rs` and
`adapter.rs` in `src-tauri/src/engines/<id>/`, register it in `runtime/registry.rs`, add a contract fixture with
`contract_tests!`, a compose file and seed in `dev/`, `ci/engines/<id>.json`, and a UI module in `src/engines/<id>/`.

## Pull requests

Keep changes focused, describe what changed and how you tested it (which engines, which checks), and include screenshots for UI changes.
Add a line for user-facing changes under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md).

## Releasing

Fosta Studio follows [Semantic Versioning](https://semver.org/). `package.json` is the source of truth; `scripts/version.mjs` keeps
`package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` in step, and CI fails when they disagree.

1. Bump the version: `npm run version:set -- 0.2.0` (or `patch`, `minor`, `major`).
2. In `CHANGELOG.md`, move the `## [Unreleased]` notes under a new `## [0.2.0] - YYYY-MM-DD` heading, leave an empty `Unreleased`
   section, and update the compare links at the bottom.
3. Commit (`Release 0.2.0`), then tag and push the tag:

   ```sh
   git tag v0.2.0
   git push origin main v0.2.0
   ```

4. The tag starts `.github/workflows/release.yml`: it checks that the tag matches the version files and that `CHANGELOG.md` has a section
   for it, builds the NSIS installer and the portable exe, and creates a **draft** GitHub Release with that section as its notes. When a
   `Dockerfile` exists, it also pushes `ghcr.io/caothanhphatit/fosta-studio` tagged `X.Y.Z`, `X.Y` and `latest`.
   The installer is signed for the in-app updater with the `TAURI_SIGNING_PRIVATE_KEY` repository secret (the key matching
   `plugins.updater.pubkey` in `tauri.conf.json`; `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` only if the key has a password), and
   `latest.json` is attached for the updater. The workflow stops early when the key secret is missing.
5. Review the draft release (notes, assets), then publish it. Installed apps see it once it is published. A tag with a suffix such as `v0.2.0-rc.1` becomes a pre-release and does not
   move the `X.Y` or `latest` image tags.
