import { useCallback, useMemo, useState } from "react";
import { isTerminalTransferState, mergeCanonicalTransfer } from "../transfers/transferState";
import type { TerminalTransferProgress, TerminalTransferScope } from "./terminalTransfers";

export interface TerminalTransferRecord {
  key: string;
  owner: TerminalTransferScope;
  progress: TerminalTransferProgress;
}

export interface TerminalTransferRegistry {
  records: readonly TerminalTransferRecord[];
  record(owner: TerminalTransferScope, progress: TerminalTransferProgress): void;
  /**
   * Clears this scope's finished-and-delivered records.
   *
   * Called by the surface at the two moments a completion stops being news:
   * once the preflight it describes has become an upload, and once the upload's
   * destination has been pasted into the pane. A successful transfer's result
   * *is* the path in the terminal, where the user is already looking, so
   * leaving a "Completed" card behind put a permanent, uncloseable notice in
   * the corner of the window for every pasted image.
   *
   * Deliberately not a rule inside `record`. A transfer can complete and still
   * never be delivered — the scope changed between the last byte and the paste
   * — and that record is the only sign the user has that bytes landed on a host
   * they are no longer looking at. Only the caller knows which happened.
   */
  dismissDelivered(owner: TerminalTransferScope, transferIds: readonly string[]): void;
  /** Clears one record the user has read; only a finished one can be cleared. */
  dismiss(key: string): void;
  markVerifying(key: string): void;
}

export function terminalTransferRecordKey(owner: TerminalTransferScope, transferId: string): string {
  return [
    owner.clientId,
    owner.hostProfileId,
    owner.serverIdentity,
    owner.connectionEpoch,
    owner.paneId,
    transferId,
  ].join("\0");
}

export function useTerminalTransferRegistry(): TerminalTransferRegistry {
  const [records, setRecords] = useState<readonly TerminalTransferRecord[]>([]);
  const record = useCallback((owner: TerminalTransferScope, progress: TerminalTransferProgress) => {
    const key = terminalTransferRecordKey(owner, progress.id);
    setRecords((current) => {
      const index = current.findIndex((item) => item.key === key);
      const next: TerminalTransferRecord = index < 0
        ? { key, owner: { ...owner }, progress }
        : { ...current[index], progress: mergeCanonicalTransfer(current[index].progress, progress) };
      if (index < 0) return [next, ...current].slice(0, 100);
      const copy = [...current];
      copy[index] = next;
      return copy;
    });
  }, []);
  const dismissDelivered = useCallback((owner: TerminalTransferScope, transferIds: readonly string[]) => {
    // By transfer id, not by owner alone: a pane's earlier batch can hold a
    // completion whose paste was refused, and that record is the only sign the
    // user has that bytes landed on a host. The next paste into the same pane
    // must not take it with it.
    const delivered = new Set(transferIds.map((id) => terminalTransferRecordKey(owner, id)));
    setRecords((current) => current.filter((item) => !(
      delivered.has(item.key) && deliveredCleanly(item.progress)
    )));
  }, []);
  const dismiss = useCallback((key: string) => {
    // Only a finished transfer, so "Dismiss" can never be a way to lose sight of
    // one that is still moving bytes.
    setRecords((current) => current.filter((item) => !(item.key === key && isTerminalTransferState(item.progress.state))));
  }, []);
  const markVerifying = useCallback((key: string) => {
    setRecords((current) => current.map((item) => item.key === key && !isTerminalTransferState(item.progress.state)
      ? { ...item, progress: { ...item.progress, state: "verifying" } }
      : item));
  }, []);
  return useMemo(
    () => ({ records, record, dismiss, dismissDelivered, markVerifying }),
    [dismiss, dismissDelivered, markVerifying, record, records],
  );
}

/**
 * Whether a transfer finished with nothing left on the host to say.
 *
 * Keyed on `cleanupStatus`, the canonical field, and not on `cleanupError`:
 * the host reports `retained`, `failed` or `connectionClosed` whether or not it
 * also supplied a message, and a completed upload that left staging bytes
 * behind is a success the user still has to be told about.
 */
function deliveredCleanly(progress: TerminalTransferProgress): boolean {
  return progress.state === "completed"
    && !progress.cleanupError
    && (progress.cleanupStatus === undefined
      || progress.cleanupStatus === "notNeeded"
      || progress.cleanupStatus === "removed");
}
