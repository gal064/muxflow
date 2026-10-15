import { useLayoutEffect } from "react";
import { applyTheme, followSystemTheme, rememberTheme, type ThemePreference } from "../../ui/appearance";

/**
 * Puts the saved theme on screen and keeps it there.
 *
 * Waits for `ready`: until the saved state has settled, `main.tsx` has already
 * applied the remembered choice, and applying the defaults over it would flash
 * the other theme for a frame.
 */
export function useAppearance(theme: ThemePreference, ready: boolean): void {
  useLayoutEffect(() => {
    if (!ready) return;
    applyTheme(theme);
    rememberTheme(theme);
    return followSystemTheme(theme);
  }, [ready, theme]);
}
