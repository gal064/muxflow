import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Platform } from "../../commands/registry";
import { recordIncident } from "../../diagnostics/incidents";

/** See `src-tauri/src/linux_window.rs`, which decides this. */
export type WindowChromeMode = "native" | "buttons" | "bare";

interface WindowChrome {
  mode: WindowChromeMode;
  desktop: string;
  reason: string;
}

/** The window buttons Muxflow's own title bar draws, when it draws any. */
export interface WindowControls {
  maximized: boolean;
  onMinimize(): void;
  onToggleMaximize(): void;
  onClose(): void;
}

type WindowAction = "minimize" | "toggleMaximize" | "close";

/**
 * The Linux title bar's window buttons, or undefined where there are none: on
 * macOS, on a tiling desktop, and when GTK's own bar is in charge.
 *
 * Close goes through `close()`, not `destroy()`, so it raises the same
 * close-requested event the platform's own close does and the app state is
 * flushed before the window goes (`usePersistedAppState`).
 */
export function useWindowChrome(platform: Platform): WindowControls | undefined {
  const [mode, setMode] = useState<WindowChromeMode>("native");
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (platform !== "linux") return;
    let cancelled = false;
    invoke<WindowChrome | undefined>("window_chrome").then((chrome) => {
      if (cancelled || !chrome) return;
      recordIncident("window.chrome", { mode: chrome.mode, desktop: chrome.desktop, reason: chrome.reason });
      setMode(chrome.mode);
    }).catch((error: unknown) => {
      // Decorations may already be off, so a window without buttons is worth
      // a line: it would otherwise be a floating window nobody can close.
      recordIncident("window.chromeUnknown", { error: String(error).slice(0, 200) });
    });
    return () => { cancelled = true; };
  }, [platform]);

  useEffect(() => {
    if (mode !== "buttons") return;
    const appWindow = getCurrentWindow();
    let cancelled = false;
    const sync = () => {
      appWindow.isMaximized().then((value) => { if (!cancelled) setMaximized(value); }).catch(() => undefined);
    };
    sync();
    const unlisten = appWindow.onResized(sync);
    return () => {
      cancelled = true;
      void unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, [mode]);

  const perform = useCallback((action: WindowAction) => {
    getCurrentWindow()[action]().catch((error: unknown) => {
      recordIncident("window.controlFailed", { action, error: String(error).slice(0, 200) });
    });
  }, []);

  if (mode !== "buttons") return undefined;
  return {
    maximized,
    onMinimize: () => perform("minimize"),
    onToggleMaximize: () => perform("toggleMaximize"),
    onClose: () => perform("close"),
  };
}
