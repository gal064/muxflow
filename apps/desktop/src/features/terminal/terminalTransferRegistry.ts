import { useCallback, useMemo, useState } from "react";
import { mergeCanonicalTransfer } from "../transfers/transferState";
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
  dismissDelivered(owner: TerminalTransferScope): void;
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
  const dismissDelivered = useCallback((owner: TerminalTransferScope) => {
    setRecords((current) => current.filter((item) => !(
      sameOwner(item.owner, owner)
      && item.progress.state === "completed"
      // A cleanup failure leaves staging bytes on the host; that is not
      // delivered, it is a success with something still to say.
      && !item.progress.cleanupError
    )));
  }, []);
  const dismiss = useCallback((key: string) => {
    // Only a finished transfer, so "Dismiss" can never be a way to lose sight of
    // one that is still moving bytes.
    setRecords((current) => current.filter((item) => !(item.key === key && isTerminal(item.progress))));
  }, []);
  const markVerifying = useCallback((key: string) => {
    setRecords((current) => current.map((item) => item.key === key && !isTerminal(item.progress)
      ? { ...item, progress: { ...item.progress, state: "verifying" } }
      : item));
  }, []);
  return useMemo(
    () => ({ records, record, dismiss, dismissDelivered, markVerifying }),
    [dismiss, dismissDelivered, markVerifying, record, records],
  );
}

function sameOwner(left: TerminalTransferScope, right: TerminalTransferScope): boolean {
  return left.clientId === right.clientId
    && left.hostProfileId === right.hostProfileId
    && left.serverIdentity === right.serverIdentity
    && left.connectionEpoch === right.connectionEpoch
    && left.paneId === right.paneId;
}

function isTerminal(progress: TerminalTransferProgress): boolean {
  return progress.state === "completed" || progress.state === "cancelled" || progress.state === "failed";
}
