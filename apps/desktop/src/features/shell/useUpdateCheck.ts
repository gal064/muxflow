import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";

import { editorFlushRegistry } from "../files/editorFlushRegistry";

/** A published release newer than the running app, as `check_for_update` returns it. */
export interface AvailableUpdate {
  version: string;
  /** The latest GitHub release page, independent of the last observed version. */
  url: string;
}

/** Once at launch, then daily: a release is at most a day late to be noticed. */
export const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

interface UpdateCheckDeps {
  check(): Promise<AvailableUpdate | null>;
  setInterval(callback: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
}

const tauriDeps: UpdateCheckDeps = {
  check: () => invoke<AvailableUpdate | null>("check_for_update"),
  setInterval: (callback, ms) => window.setInterval(callback, ms),
  clearInterval: (handle) => window.clearInterval(handle as number),
};

/**
 * Starts the periodic check and reports every answer. A failed check (offline,
 * GitHub unreachable, an unreadable manifest) reports nothing and leaves the
 * last answer standing: it is the next successful check that decides, never a
 * network error. Returns the stop function.
 */
export function startUpdateCheck(onResult: (update: AvailableUpdate | null) => void, deps: UpdateCheckDeps = tauriDeps): () => void {
  let stopped = false;
  const run = () => {
    deps.check().then((update) => { if (!stopped) onResult(update); }, () => undefined);
  };
  run();
  const handle = deps.setInterval(run, UPDATE_CHECK_INTERVAL_MS);
  return () => {
    stopped = true;
    deps.clearInterval(handle);
  };
}

export interface InstallUpdateDeps {
  confirm(message: string): Promise<boolean>;
  /** Installs, then closes the window through its normal save path; the exit relaunches. */
  install(): Promise<void>;
  openReleasePage(url: string): Promise<void>;
  setStatus(message: string): void;
}

export const tauriInstallDeps = {
  confirm: (message: string) => ask(message, { title: "Update Muxflow", okLabel: "Update and restart" }),
  install: async () => {
    // Before anything is replaced: a save that fails here leaves the app as it was.
    await editorFlushRegistry.flushAll();
    await invoke<void>("install_update");
    await getCurrentWindow().close();
  },
};

let installing = false;

/**
 * Installs the offered release once the user agrees to the restart. On any
 * failure the release page is the way to update.
 */
export async function installUpdate(update: AvailableUpdate, deps: InstallUpdateDeps): Promise<void> {
  if (installing) return;
  installing = true;
  try {
    if (!await deps.confirm(`Install Muxflow ${update.version} and restart? Port forwards and file transfers in progress will stop.`)) return;
    deps.setStatus(`Updating to Muxflow ${update.version}…`);
    await deps.install();
  } catch (error) {
    deps.setStatus(String(error));
    await deps.openReleasePage(update.url);
  } finally {
    installing = false;
  }
}

/** The release to point at, or null while the running app is the newest. */
export function useUpdateCheck(): AvailableUpdate | null {
  const [update, setUpdate] = useState<AvailableUpdate | null>(null);
  useEffect(() => {
    // The browser dev harness has no shell to ask.
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
    return startUpdateCheck(setUpdate);
  }, []);
  return update;
}
