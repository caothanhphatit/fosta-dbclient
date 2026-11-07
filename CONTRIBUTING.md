# Contributing to Fosta Studio

Thanks for helping. This guide covers the local setup, the checks CI runs, and the conventions of the code base.
[ARCHITECTURE.md](ARCHITECTURE.md) explains how the pieces fit together.

> **License and CLA**: Fosta Studio is licensed under the [GPL v3.0](LICENSE). On your first pull request a bot asks you to sign the
> [Contributor License Agreement](CLA.md) by posting a comment; you keep the copyright in your work, and the CLA lets the
> maintainer also offer Fosta Studio under commercial terms.

## Project layout

```
src/                     TypeScript UI (no framework, bundled by Vite)
  main.ts                app shell: layout, wiring, current config
  app/                   tab navigation, welcome screen, assistant context
  core/                  shared building blocks: IPC client (api.ts), types, DOM helpers, dialogs, tabs, theme, SQL helpers
  features/<area>/       one folder per feature (connections, workspaces, tasks, grid, table, query, design,
                         diagram, vector, assistant, importExport), each with its own dialogs
  dev/devMock.ts         in-browser mock backend for `npm run dev`
  styles.css             all styles
src-tauri/src/           Rust backend
  lib.rs                 the shared backend: AppState, Capabilities, module list
  main.rs                desktop front-end: Tauri setup and the command wrappers
  server/                web front-end (feature `web`): HTTP API, sign-in, static UI
  commands/              command bodies, grouped by area; for_each_command! lists them
  drivers/               Driver trait + MariaDB/MySQL, SQL Server and MongoDB drivers, shared types
  ai/                    assistant: prompt, tools, Anthropic and OpenAI wire formats
  config.rs secrets.rs   fosta-studio.json, Credential Manager / encrypted secret file
  tasks.rs job.rs        startup tasks and their child processes
  schema.rs vector.rs    ER diagram data, MariaDB vector tools
dev/                     docker-compose.yml and seed data for the test databases
scripts/                 toolchain installer and icon generator (PowerShell)
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
npm run dev            # UI only, in a browser, against the mock backend (src/dev/devMock.ts)
npm run tauri build -- --no-bundle   # portable src-tauri\target\release\fosta-studio.exe
```

The mock backend is loaded only when Vite runs in dev mode and the page is not inside Tauri, so it never reaches the desktop build. Keep it
roughly in step with the real commands when you change their shapes.

## Checks

CI (`.github/workflows/ci.yml`, Windows) runs all of these; run them before opening a pull request:

```sh
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

## Test databases and live tests

`dev/docker-compose.yml` starts MariaDB 11.8 (`shop`, with a vector column), SQL Server 2022 (`ERP`) and MongoDB 8 (`events`), all with
generated sample data. The credentials in that file are for local testing only.

```sh
docker compose -f dev/docker-compose.yml up -d
sh dev/seed-mssql.sh    # SQL Server has no init directory; this loads dev/seed/mssql.sql once
cd src-tauri
cargo test live_ -- --ignored --test-threads=1
```

Notes on the live tests (`src-tauri/src/live_tests.rs`):

- `live_saved_passwords_are_readable` expects saved passwords in Credential Manager for connections with the ids `local-mariadb`,
  `local-mssql` and `local-mongo` (create them in the app with "Save password" ticked, or skip this test).
- `live_assistant_tools` talks to the Fosta API and needs `FOSTA_KEY` in the environment; without it the test prints a note and passes.
- They live inside the library crate (as a `#[cfg(test)]` module in `lib.rs`) because they use crate-internal APIs.

## Code style

- **Formatting**: `cargo fmt` with `src-tauri/rustfmt.toml` (140 columns). Prettier for TypeScript and CSS (`.prettierrc.json`: double
  quotes, semicolons, 2 spaces, 160 columns, trailing commas): `npm run format` / `npm run format:check`. `.editorconfig` covers editors.
- **TypeScript**: strict mode, no framework. Build DOM with `h()` from `core/dom.ts`; talk to the backend only through `core/api.ts`.
  Feature code goes under `src/features/<area>/`, dialogs next to the feature that opens them. Keep heavy dependencies out of the main
  chunk: the query view (CodeMirror) and the dev mock are loaded with dynamic `import()`.
- **Rust**: errors are `AppError` strings shown to the user, so write them as sentences that say what to do. Serialised types use
  `#[serde(rename_all = "camelCase")]` and must match `src/core/types.ts`.
- **Compatibility**: Tauri command names and argument names, the JSON shapes, the `fosta-studio.json` layout, the keyring service
  `fosta-studio` and its account names, and the `db-studio.*` localStorage keys are a contract with existing installs. Change them only with
  a migration.
- Unit-test pure logic (parsers, SQL builders) next to the code: `foo.test.ts` with Vitest, `#[cfg(test)] mod tests` in Rust.

## Adding a database driver

The checklist is in [ARCHITECTURE.md](ARCHITECTURE.md#how-to-add-a-database-driver). In short: implement `Driver` in
`src-tauri/src/drivers/<engine>.rs`, route the new driver id in `drivers::open`, describe it in `src/core/drivers.ts`, teach
`connectionString.ts` its string format, add sample data to `dev/` and a live test.

## Pull requests

Keep changes focused, describe what changed and how you tested it (which engines, which checks), and include screenshots for UI changes.
