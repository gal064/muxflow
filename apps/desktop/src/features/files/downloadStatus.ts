import { isTerminalTransferState } from "../transfers/transferState";
import type { TransferStatus } from "./types";

export interface ActiveDownloadStatus {
  id: string;
  path: string;
  banner: string;
}

/** A finished download the user can now open, and the message announcing it. */
export interface DownloadCompletion {
  destination: string;
  message: string;
}

export function reconcileDownloadStatus(
  current: string,
  active: ActiveDownloadStatus | undefined,
  transfers: readonly TransferStatus[],
): { status: string; active?: ActiveDownloadStatus; completion?: DownloadCompletion } {
  if (!active) return { status: current };
  const transfer = transfers.find((candidate) => candidate.id === active.id);
  if (!transfer || !isTerminalTransferState(transfer.state)) return { status: current, active };
  if (current !== active.banner) return { status: current };
  // A success names the file the user now has. The remote source path is the
  // one thing they cannot act on, and it was also a lie whenever the backend
  // published under a different name.
  if (transfer.state === "completed" && transfer.destination) {
    const message = `Download complete: ${transfer.destination}`;
    return { status: message, completion: { destination: transfer.destination, message } };
  }
  return { status: `Download ${transfer.state}: ${active.path}` };
}
