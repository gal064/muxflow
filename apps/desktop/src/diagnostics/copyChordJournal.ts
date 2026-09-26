import type { ShortcutDisposition } from "../commands/registry";
import { recordIncident } from "./incidents";

/**
 * How long after a Linux Ctrl+C copy a second Ctrl+C still reads as the user
 * having wanted the interrupt the first time.
 */
export const COPY_THEN_INTERRUPT_MS = 1_500;

/**
 * Journals what Linux Ctrl+C did, since one chord is both copy and interrupt.
 *
 * `terminal.copyChord` records each copy. `terminal.copyThenInterrupt` records
 * a copy followed quickly by an interrupt in the same pane — the signature of a
 * leftover selection having swallowed an interrupt, which is the failure the
 * selection gate exists to prevent and so the one worth counting.
 */
export function createCopyChordJournal(
  record: typeof recordIncident = recordIncident,
  now: () => number = () => performance.now(),
) {
  let lastCopy: { paneId: string; at: number } | undefined;
  return (disposition: ShortcutDisposition, paneId: string | undefined): void => {
    if (disposition.kind !== "run" && disposition.kind !== "yield") return;
    if (disposition.commandId !== "terminal.copy" || !paneId) return;
    if (disposition.kind === "run") {
      lastCopy = { paneId, at: now() };
      record("terminal.copyChord", { paneId });
      return;
    }
    const previous = lastCopy;
    lastCopy = undefined;
    if (!previous || previous.paneId !== paneId) return;
    const ms = Math.round(now() - previous.at);
    if (ms <= COPY_THEN_INTERRUPT_MS) record("terminal.copyThenInterrupt", { paneId, ms });
  };
}
