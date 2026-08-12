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
  dismissSuccessfulPreflights(owner: TerminalTransferScope): void;
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
  const dismissSuccessfulPreflights = useCallback((owner: TerminalTransferScope) => {
    setRecords((current) => current.filter((item) => !(
      sameOwner(item.owner, owner)
      && item.progress.state === "completed"
      && item.progress.outcome === "notPublished"
    )));
  }, []);
  const markVerifying = useCallback((key: string) => {
    setRecords((current) => current.map((item) => item.key === key && !isTerminal(item.progress)
      ? { ...item, progress: { ...item.progress, state: "verifying" } }
      : item));
  }, []);
  return useMemo(() => ({ records, record, dismissSuccessfulPreflights, markVerifying }), [dismissSuccessfulPreflights, markVerifying, record, records]);
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
