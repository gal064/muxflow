import { useEffect, useState } from "react";
import type { SaveState } from "./autosave";

export type VisibleSaveState = Extract<SaveState, "dirty" | "saving" | "error">;

export const SAVING_STATUS_DELAY_MILLIS = 100;

/**
 * Turns the autosave controller's exact state into a calmer status for people.
 *
 * A healthy autosave's debounce-sized dirty interval is implementation state,
 * not useful status. Dirty becomes visible only when writes are unavailable;
 * errors remain immediate, and saving appears only when the write is slow
 * enough to notice. A successful save clears the status.
 */
export function useVisibleSaveState(
  state: SaveState | undefined,
  writeAvailable: boolean,
  savingDelayMillis = SAVING_STATUS_DELAY_MILLIS,
): VisibleSaveState | undefined {
  const [savingVisible, setSavingVisible] = useState(false);

  useEffect(() => {
    if (state !== "saving") {
      setSavingVisible(false);
      return;
    }

    const timer = setTimeout(() => setSavingVisible(true), savingDelayMillis);
    return () => clearTimeout(timer);
  }, [savingDelayMillis, state]);

  return projectVisibleSaveState(state, savingVisible, writeAvailable);
}

export function projectVisibleSaveState(
  state: SaveState | undefined,
  savingVisible: boolean,
  writeAvailable: boolean,
): VisibleSaveState | undefined {
  if (state === "saving") return savingVisible ? "saving" : undefined;
  if (state === "dirty") return writeAvailable ? undefined : "dirty";
  return state === "error" ? state : undefined;
}
