import { invoke } from "@tauri-apps/api/core";
import { useEffect, useLayoutEffect } from "react";
import { applyInterfaceScale, applyTheme, followSystemTheme, rememberTheme, type ThemePreference } from "../../ui/appearance";

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

/**
 * Applies the saved interface zoom when it changes. Startup needs nothing from
 * here: the backend zooms the window from the saved state before the page
 * loads, so this only ever re-sends what is already applied, or a new choice.
 */
export function useInterfaceScale(scale: number, ready: boolean): void {
  useEffect(() => {
    if (!ready) return;
    applyInterfaceScale(scale);
    void invoke("set_ui_scale", { scale })
      .catch((error: unknown) => console.warn("could not set the interface size", error));
  }, [ready, scale]);
}
