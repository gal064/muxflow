/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  base: "./",
  plugins: [react()],
  clearScreen: false,
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
});
