import { useEffect, useState } from "react";
import type { SaveState } from "./autosave";

export type VisibleSaveState = Exclude<SaveState, "saved">;

export const SAVING_STATUS_DELAY_MILLIS = 100;

/**
 * Turns the autosave controller's exact state into a calmer status for people.
 *
 * Dirty and error states matter immediately. Saving is useful only when the
 * write is slow enough to notice, so the existing dirty label stays in place
 * briefly while that state settles. A successful save clears the status rather
 * than flashing a confirmation after every edit.
 */
export function useVisibleSaveState(
  state: SaveState | undefined,
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

  return projectVisibleSaveState(state, savingVisible);
}

export function projectVisibleSaveState(
  state: SaveState | undefined,
  savingVisible: boolean,
): VisibleSaveState | undefined {
  if (state === "saving") return savingVisible ? "saving" : "dirty";
  return state === "dirty" || state === "error" ? state : undefined;
}
