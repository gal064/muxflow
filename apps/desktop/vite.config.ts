/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ command }) => ({
  base: "./",
  plugins: [react()],
  clearScreen: false,
  define: {
    // Development carries the same instrumentation as Rust's
    // `debug_assertions` build. Production carries it only in an explicit
    // measurement package, allowing Rollup to erase its hot-path call sites
    // from an ordinary release rather than merely branching around them.
    __MUXFLOW_PERF_BUILD__: JSON.stringify(
      command === "serve" || process.env.MUXFLOW_PERF_BUILD === "1",
    ),
  },
  test: {
    // Vitest stubs CSS imports to empty strings by default. The token file is
    // the app's single source of colors and metrics, and one test asserts that
    // the terminal renderer's no-stylesheet fallback still agrees with it — so
    // that test has to be able to read the real file.
    css: true,
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
