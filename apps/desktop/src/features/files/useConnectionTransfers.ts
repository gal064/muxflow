import { useCallback, useEffect, useState } from "react";
import { keyForTransferConnection } from "./api";
import { isTerminalTransferState, mergeCanonicalTransfer } from "../transfers/transferState";
import type { FileWorkspaceScope, TransferStatus } from "./types";

/** How many transfers one connection keeps in view. */
const MAX_LISTED_TRANSFERS = 100;

/**
 * The download transfers belonging to one live connection.
 *
 * Separate from the Explorer's own state because a transfer has nothing to do
 * with watching a filesystem: it outlives pane, root, and session selection,
 * and it is answered by the host on a different lane entirely. Keeping it
 * inside the directory hook meant every pane switch had to remember not to
 * throw it away, in three places.
 *
 * The connection is what a transfer belongs to. When that changes, anything
 * still in flight loses the connection that would have told it how it ended —
 * so it is marked failed with the outcome honestly unknown rather than left
 * looking like it is still running.
 */
export function useConnectionTransfers(scope: FileWorkspaceScope | undefined) {
  const connectionKey = scope ? keyForTransferConnection(scope) : "";
  const [held, setHeld] = useState<{ connectionKey: string; transfers: readonly TransferStatus[] }>(
    { connectionKey: "", transfers: [] },
  );

  useEffect(() => {
    setHeld((current) => current.connectionKey === connectionKey
      ? current
      : { connectionKey, transfers: replaced(current.connectionKey, current.transfers) });
  }, [connectionKey]);

  const record = useCallback((transfer: TransferStatus) => {
    if (transfer.scopeKey !== connectionKey) return;
    setHeld((current) => current.connectionKey === connectionKey
      ? { ...current, transfers: upsertTransfer(current.transfers, transfer) }
      : { connectionKey, transfers: [transfer] });
  }, [connectionKey]);

  const clearFinishedTransfers = useCallback(() => {
    setHeld((current) => {
      const transfers = current.connectionKey === connectionKey
        ? current.transfers
        : replaced(current.connectionKey, current.transfers);
      return {
        connectionKey,
        transfers: transfers.filter((transfer) => !isTerminalTransferState(transfer.state)),
      };
    });
  }, [connectionKey]);

  // Effects run after paint, so the committed key can be one render behind.
  // Masking here rather than waiting means the previous connection's transfers
  // never appear as live under a connection that cannot finish them.
  const transfers = held.connectionKey === connectionKey
    ? held.transfers
    : replaced(held.connectionKey, held.transfers);
  return { connectionKey, transfers, record, clearFinishedTransfers };
}

/** A first connection adopts what is there; a replacement retires it. */
function replaced(previousKey: string, transfers: readonly TransferStatus[]): readonly TransferStatus[] {
  return previousKey === "" ? transfers : transfers.map(staleTransferOnScopeReplacement);
}

function staleTransferOnScopeReplacement(transfer: TransferStatus): TransferStatus {
  if (isTerminalTransferState(transfer.state)) return transfer;
  return {
    ...transfer,
    state: "failed",
    outcome: transfer.state === "verifying" ? "unknown" : "notPublished",
    failureKind: "staleScope",
    error: "Transfer scope was replaced before its authoritative terminal event arrived.",
  };
}

function upsertTransfer(transfers: readonly TransferStatus[], next: TransferStatus): TransferStatus[] {
  const index = transfers.findIndex((item) => item.id === next.id);
  if (index < 0) return [next, ...transfers].slice(0, MAX_LISTED_TRANSFERS);
  const copy = [...transfers];
  copy[index] = mergeCanonicalTransfer(copy[index], next);
  return copy;
}
