import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // The live tests build and drive the real helper; the cap is for a cold cargo build.
    testTimeout: 120_000,
    hookTimeout: 600_000,
  },
});
