import path from "node:path";
import { defineConfig } from "vitest/config";

const stub = (file: string) => path.resolve(__dirname, "src/testing/stubs", file);

export default defineConfig({
  esbuild: { jsx: "automatic" },
  resolve: {
    // Screens render in Node with react-test-renderer; these stand in for the
    // native modules they import (src/testing/stubs).
    alias: [
      { find: /^react-native$/, replacement: stub("reactNative.tsx") },
      { find: /^react-native-svg$/, replacement: stub("reactNativeSvg.tsx") },
      { find: /^react-native-reanimated$/, replacement: stub("reanimated.tsx") },
      { find: /^react-native-safe-area-context$/, replacement: stub("safeArea.tsx") },
      { find: /^expo-router$/, replacement: stub("expoRouter.tsx") },
      { find: /^expo-image-picker$/, replacement: stub("expoImagePicker.ts") },
      { find: /^expo-document-picker$/, replacement: stub("expoDocumentPicker.ts") },
    ],
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    // The live tests build and drive the real helper; the cap is for a cold cargo build.
    testTimeout: 120_000,
    hookTimeout: 600_000,
  },
});
