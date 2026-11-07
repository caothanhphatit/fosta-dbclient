import { defineConfig } from "vitest/config";

// Unit tests for the pure frontend logic (parsers, filters, SQL helpers); no DOM needed.
export default defineConfig({
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
