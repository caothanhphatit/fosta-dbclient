// ESLint flat config: recommended JavaScript + typescript-eslint rules for the frontend.
// Formatting is left to Prettier (see .prettierrc.json).
import js from "@eslint/js";
import { defineConfig } from "eslint/config";
import tseslint from "typescript-eslint";

export default defineConfig(
  // dev/seed holds mongosh scripts with their own globals.
  { ignores: ["dist/", "node_modules/", "src-tauri/", "dev/", "website/"] },
  js.configs.recommended,
  tseslint.configs.recommended,
  // Node scripts (scripts/*.mjs) run outside the browser.
  { files: ["scripts/**/*.mjs"], languageOptions: { globals: { process: "readonly", console: "readonly" } } },
);
