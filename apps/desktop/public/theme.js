// A classic script in <head>, so it runs before the first paint: the module
// bundle runs too late to spare a light theme the dark canvas the window shows
// first. The CSP allows no inline script, hence a file. `applyTheme` in
// src/ui/appearance.ts owns this attribute from here on; this mirrors its rule.
//
// The saved choice comes from the backend, which injects `__muxflowBoot` from
// the app state before any page script runs (`initialization_script` in
// src-tauri/src/lib.rs). There is no copy of it in storage.
try {
  const boot = window.__muxflowBoot ?? {};
  const theme =
    boot.theme === "light" || boot.theme === "dark"
      ? boot.theme
      : matchMedia("(prefers-color-scheme: dark)").matches
        ? "dark"
        : "light";
  document.documentElement.dataset.theme = theme;
  // The interface zoom, for the lengths that divide by it (`applyInterfaceScale`).
  const scale = Number(boot.uiScale);
  if (scale > 0) document.documentElement.style.setProperty("--ui-scale", String(scale));
} catch {
  // No media API: main.tsx applies the theme a moment later.
}
