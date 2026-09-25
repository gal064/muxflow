import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

/** A published release newer than the running app, as `check_for_update` returns it. */
export interface AvailableUpdate {
  version: string;
  /** Its GitHub release page; the shell only ever returns one under the Muxflow releases. */
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
