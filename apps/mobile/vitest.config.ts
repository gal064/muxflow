import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["src/**/*.test.ts"],
    // The live host test builds and drives the real helper; everything else is
    // sub-second. The cap is for the cargo build on a cold cache.
    testTimeout: 120_000,
    hookTimeout: 600_000,
  },
});
