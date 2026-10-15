// A classic script in <head>, so it runs before the first paint: the module
// bundle runs too late to spare a light theme the dark canvas the window shows
// first. The CSP allows no inline script, hence a file. `applyTheme` in
// src/ui/appearance.ts owns this attribute from here on; this mirrors its rule.
try {
  const saved = localStorage.getItem("muxflow.theme");
  const theme =
    saved === "light" || saved === "dark"
      ? saved
      : matchMedia("(prefers-color-scheme: dark)").matches
        ? "dark"
        : "light";
  document.documentElement.dataset.theme = theme;
} catch {
  // No storage or media API: main.tsx applies the theme a moment later.
}
