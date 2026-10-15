/**
 * Which palette is on screen.
 *
 * The colors themselves are CSS: the generated palette declares dark on bare
 * `:root` and light under `data-theme="light"`. This module only decides that
 * attribute. Stylesheet rules follow it on their own; the terminal and the
 * editor take colors as values, so they subscribe and re-read.
 */

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "muxflow.theme";
const SYSTEM_DARK_QUERY = "(prefers-color-scheme: dark)";
const listeners = new Set<() => void>();

export function isThemePreference(value: unknown): value is ThemePreference {
  return value === "system" || value === "light" || value === "dark";
}

export function resolvedTheme(theme: ThemePreference, systemDark: boolean): ResolvedTheme {
  if (theme === "system") return systemDark ? "dark" : "light";
  return theme;
}

/** The theme on screen. A root with no attribute is dark, as the CSS is. */
export function currentTheme(root: Element | undefined = globalThis.document?.documentElement): ResolvedTheme {
  return root?.getAttribute("data-theme") === "light" ? "light" : "dark";
}

export function systemPrefersDark(): boolean {
  // No media API means no OS preference to follow: stay on the app's original theme.
  return globalThis.matchMedia?.(SYSTEM_DARK_QUERY).matches ?? true;
}

/** Calls `listener` whenever `applyTheme` changes what is on screen. */
export function onAppearanceChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function applyTheme(
  theme: ThemePreference,
  systemDark = systemPrefersDark(),
  root: HTMLElement | undefined = globalThis.document?.documentElement,
): void {
  if (!root) return;
  const resolved = resolvedTheme(theme, systemDark);
  if (root.dataset.theme === resolved) return;
  root.dataset.theme = resolved;
  for (const listener of listeners) listener();
}

/**
 * Follows the OS while the preference is "system". Returns the unsubscribe.
 *
 * WebKit reports the OS appearance through this media query on both macOS and
 * Linux, and fires `change` when it flips, so a scheduled light/dark switch
 * repaints the app without a restart.
 */
export function followSystemTheme(theme: ThemePreference): () => void {
  const query = globalThis.matchMedia?.(SYSTEM_DARK_QUERY);
  if (!query || theme !== "system") return () => undefined;
  const update = () => applyTheme(theme, query.matches);
  query.addEventListener("change", update);
  return () => query.removeEventListener("change", update);
}

/**
 * Saved state arrives over IPC after the first paint, so the last choice is
 * also kept here, where `main.tsx` can apply it synchronously and a light-theme
 * user does not see a dark frame on every launch.
 */
export function rememberTheme(theme: ThemePreference, storage: Pick<Storage, "setItem"> | undefined = globalThis.localStorage): void {
  try {
    storage?.setItem(STORAGE_KEY, theme);
  } catch {
    // Storage is a startup nicety; the saved app state stays authoritative.
  }
}

export function rememberedTheme(storage: Pick<Storage, "getItem"> | undefined = globalThis.localStorage): ThemePreference | undefined {
  try {
    const value = storage?.getItem(STORAGE_KEY);
    return isThemePreference(value) ? value : undefined;
  } catch {
    return undefined;
  }
}
