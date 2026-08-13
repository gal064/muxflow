import { isTerminalTransferState } from "../transfers/transferState";
import type { TransferStatus } from "./types";

export interface ActiveDownloadStatus {
  id: string;
  path: string;
  banner: string;
}

export function reconcileDownloadStatus(
  current: string,
  active: ActiveDownloadStatus | undefined,
  transfers: readonly TransferStatus[],
): { status: string; active?: ActiveDownloadStatus } {
  if (!active) return { status: current };
  const transfer = transfers.find((candidate) => candidate.id === active.id);
  if (!transfer || !isTerminalTransferState(transfer.state)) return { status: current, active };
  if (current !== active.banner) return { status: current };
  return { status: `Download ${transfer.state}: ${active.path}` };
}
