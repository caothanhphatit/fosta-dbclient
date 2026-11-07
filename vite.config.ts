import { readFileSync } from "node:fs";
import { defineConfig } from "vite";

// package.json is the version source of truth (kept in step with Tauri and Cargo by scripts/version.mjs).
const { version } = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };

export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist" },
  define: { __APP_VERSION__: JSON.stringify(version) },
});
