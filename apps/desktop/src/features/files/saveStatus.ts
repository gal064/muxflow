import { useEffect, useState } from "react";
import type { SaveState } from "./autosave";

export type VisibleSaveState = Extract<SaveState, "dirty" | "saving" | "error">;

export const SLOW_SAVE_DELAY_MILLIS = 1200;

/**
 * Turns the autosave controller's exact state into a calmer status for people.
 *
 * Typing runs the controller around a dirty → saving → saved loop several times
 * a second, and a status keyed on that loop flickers: the debounce, the write
 * latency and the stale-save downgrade back to dirty are all implementation
 * state rather than news. So the whole loop is read as one boolean — a save is
 * owed — and only an unbroken 1.2 seconds of owing anything is slow enough to
 * report. Errors stay immediate, a lost write authority still says so, and a
 * successful save says nothing at all.
 */
export function useVisibleSaveState(
  state: SaveState | undefined,
  writeAvailable: boolean,
  slowSaveDelayMillis = SLOW_SAVE_DELAY_MILLIS,
): VisibleSaveState | undefined {
  const pending = state === "dirty" || state === "saving";
  const [slowSaveVisible, setSlowSaveVisible] = useState(false);

  // Keyed on the boolean, never on `state`: the dirty ↔ saving churn of a
  // healthy autosave must not restart — or interrupt — this one timer.
  useEffect(() => {
    if (!pending) {
      setSlowSaveVisible(false);
      return;
    }

    const timer = setTimeout(() => setSlowSaveVisible(true), slowSaveDelayMillis);
    return () => clearTimeout(timer);
  }, [pending, slowSaveDelayMillis]);

  return projectVisibleSaveState(state, slowSaveVisible, writeAvailable);
}

export function projectVisibleSaveState(
  state: SaveState | undefined,
  slowSaveVisible: boolean,
  writeAvailable: boolean,
): VisibleSaveState | undefined {
  if (state === "error") return "error";
  if (state !== "dirty" && state !== "saving") return undefined;
  if (state === "dirty" && !writeAvailable) return "dirty";
  return slowSaveVisible ? "saving" : undefined;
}
