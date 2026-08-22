import { invoke } from "@tauri-apps/api/core";
import { availableMonitors, getCurrentWindow, primaryMonitor } from "@tauri-apps/api/window";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import { useCallback, useEffect, useMemo, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { AppStatePersistence } from "./appStatePersistence";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { defaultAppState, normalizePersistedAppState, type PersistedAppState } from "./types";
import { captureWindowGeometry, restoredWindowGeometry } from "./windowGeometry";
import { repairShortcutCollisions, type Platform } from "../../commands/registry";

export function usePersistedAppState(
  report: (message: string) => void,
  platform: Platform,
): {
  appState: PersistedAppState;
  appStateRecovery?: string;
  resetAppState(): Promise<void>;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
} {
  const [state, setState] = useState(defaultAppState);
  const [recovery, setRecovery] = useState<string>();
  const stateRef = useRef(state);
  stateRef.current = state;
  const loaded = useRef(false);
  const persistence = useMemo(() => new AppStatePersistence(
    (value) => invoke("save_app_state", { state: value }),
    120,
    (error) => report(`Could not save application tabs; changes will be retried: ${String(error)}`),
  ), [report]);

  useEffect(() => {
    void invoke<unknown>("load_app_state").then((saved) => {
      const restored = normalizePersistedAppState(saved);
      const shortcutOverrides = repairShortcutCollisions(
        platform,
        restored.commands.shortcutOverrides,
        (displaced) => report(`Disabled conflicting saved shortcuts: ${displaced
          .map(({ commandId, shortcut }) => `${commandId} (${shortcut})`).join(", ")}.`),
      );
      setState(shortcutOverrides === restored.commands.shortcutOverrides ? restored : {
        ...restored,
        commands: { shortcutOverrides },
      });
      loaded.current = true;
      const geometry = restored.shell.windowGeometry;
      if (geometry) {
        const appWindow = getCurrentWindow();
        void (async () => {
          const [scaleFactor, monitors, primary] = await Promise.all([
            appWindow.scaleFactor(), availableMonitors(), primaryMonitor(),
          ]);
          const restoredGeometry = restoredWindowGeometry(geometry, monitors, primary, scaleFactor);
          await appWindow.setPosition(new PhysicalPosition(restoredGeometry.x, restoredGeometry.y));
          await appWindow.setSize(new PhysicalSize(restoredGeometry.width, restoredGeometry.height));
          if (geometry.maximized) await appWindow.maximize();
        })().catch((error) => report(`Could not restore window geometry: ${String(error)}`));
      }
    }).catch((error) => {
      const message = String(error);
      setRecovery(message);
      report(message);
    });
  }, [platform, report]);

  useEffect(() => {
    if (loaded.current) persistence.schedule(state);
  }, [persistence, state]);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    const captureGeometry = async () => {
      const [position, size, maximized, scaleFactor] = await Promise.all([
        appWindow.outerPosition(), appWindow.innerSize(), appWindow.isMaximized(), appWindow.scaleFactor(),
      ]);
      setState((current) => ({
        ...current,
        shell: { ...current.shell, windowGeometry: captureWindowGeometry(position, size, maximized, scaleFactor) },
      }));
    };
    const moved = appWindow.onMoved(() => { void captureGeometry().catch((error) => report(`Could not capture window geometry: ${String(error)}`)); });
    const resized = appWindow.onResized(() => { void captureGeometry().catch((error) => report(`Could not capture window geometry: ${String(error)}`)); });
    const unlisten = appWindow.onCloseRequested(async (event) => {
      event.preventDefault();
      try {
        await editorFlushRegistry.flushAll();
        if (loaded.current) await persistence.flush(stateRef.current);
        await appWindow.destroy();
      } catch (error) {
        report(`Could not save application state; close cancelled: ${String(error)}`);
      }
    });
    return () => {
      void unlisten.then((dispose) => dispose());
      void moved.then((dispose) => dispose());
      void resized.then((dispose) => dispose());
      persistence.dispose();
    };
  }, [persistence, report]);

  const reset = useCallback(async () => {
    const value = normalizePersistedAppState(await invoke<unknown>("reset_app_state"));
    setState({
      ...value,
      commands: { shortcutOverrides: repairShortcutCollisions(
        platform,
        value.commands.shortcutOverrides,
        (displaced) => report(`Disabled conflicting reset shortcuts: ${displaced
          .map(({ commandId, shortcut }) => `${commandId} (${shortcut})`).join(", ")}.`),
      ) },
    });
    setRecovery(undefined);
    loaded.current = true;
    report("Saved shell state was reset; the invalid original was preserved for recovery.");
  }, [platform, report]);

  return { appState: state, appStateRecovery: recovery, resetAppState: reset, setAppState: setState };
}
