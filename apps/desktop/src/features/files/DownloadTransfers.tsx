import { useState } from "react";
import { DownloadActions } from "./DownloadActions";
import type { TransferStatus } from "./types";
import { canCancelTransfer, transferStateLabel } from "../transfers/transferState";

/**
 * The Explorer's downloads list.
 *
 * It sits inside the Explorer's grid but shares nothing with the tree — not a
 * root, not a selection, not a listing. Keeping it here rather than in
 * `ExplorerTree` is what stops "open the finished file" from becoming two more
 * callbacks threaded through a component that has no idea what a download is.
 *
 * Open and reveal are invoked directly for the same reason, and a failure is
 * reported on the row that caused it rather than in the app's status channel:
 * every other thing that can go wrong with a transfer is already reported
 * there, next to the file it happened to.
 */
export function DownloadTransfers({ transfers, onCancelTransfer }: {
  transfers: readonly TransferStatus[];
  onCancelTransfer(id: string): Promise<void>;
}) {
  const [openError, setOpenError] = useState<{ id: string; message: string }>();
  if (transfers.length === 0) return null;
  return <section aria-label="Downloads" className="transfers">
    <h3>Downloads</h3>
    {transfers.map((transfer) => <div aria-label={`Download ${transfer.path}: ${transferStateLabel(transfer.state)}`} className={`transfer ${transfer.state}`} key={transfer.id}>
      <span>{transfer.path.split("/").at(-1)}</span><small>{transferStateLabel(transfer.state)}</small>
      {transfer.totalBytes ? <progress aria-label={`Download progress for ${transfer.path}`} aria-valuetext={formatTransfer(transfer)} data-completed-bytes={transfer.completedBytes} data-total-bytes={transfer.totalBytes} max={1000} value={transferPermille(transfer.completedBytes, transfer.totalBytes)} /> : <progress aria-label={`Download progress for ${transfer.path}`} data-completed-bytes={transfer.completedBytes} />}
      <small className="transfer-detail">{formatTransfer(transfer)}</small>
      {canCancelTransfer(transfer.state) && <button aria-label={`Cancel download ${transfer.path}`} onClick={() => void onCancelTransfer(transfer.id)} type="button">Cancel</button>}
      {/* A published local file is the thing that can be opened — not a
          "completed" state. A transfer whose scope went stale after it
          published reports `failed` with `outcome: "published"`, and that is
          exactly when the user most needs to find the file. */}
      {transfer.outcome === "published" && transfer.destination && <div className="transfer-actions">
        <DownloadActions destination={transfer.destination} onResult={(error) => setOpenError(error ? { id: transfer.id, message: error } : undefined)} />
      </div>}
      {openError?.id === transfer.id && <em role="alert">{openError.message}</em>}
      {transfer.state === "verifying" && <small className="transfer-detail transfer-finalizing" role="status">The verified bytes are being committed; awaiting the authoritative backend outcome.</small>}
      {transfer.failureKind === "staleScope" && <em role="alert">Download stopped because the connection scope changed.</em>}
      {transfer.failureKind === "timeout" && <em role="alert">Download timed out before an authoritative result arrived.</em>}
      {transfer.outcome === "unknown" && <em role="alert">The download outcome is unknown. Inspect the destination before retrying.</em>}
      {transfer.error && <em role="alert">{transfer.error}</em>}
      {transfer.cleanupError && <em role="alert">Partial cleanup failed: {transfer.cleanupError}</em>}
      {transfer.cleanupStatus && ["failed", "cancelled"].includes(transfer.state) && <small className="transfer-detail">Cleanup: {transfer.cleanupStatus}</small>}
    </div>)}
  </section>;
}

function formatTransfer(transfer: TransferStatus): string {
  const progress = transfer.totalBytes
    ? `${formatTransferBytes(transfer.completedBytes)} / ${formatTransferBytes(transfer.totalBytes)}`
    : `${formatTransferBytes(transfer.completedBytes)} transferred`;
  const speed = transfer.bytesPerSecond ? ` · ${formatTransferBytes(transfer.bytesPerSecond)}/s` : "";
  const eta = transfer.etaSeconds !== undefined && transfer.etaSeconds > 0 ? ` · ${Math.ceil(transfer.etaSeconds)}s remaining` : "";
  return `${progress}${speed}${eta}`;
}

function formatTransferBytes(value: string): string {
  if (!/^(0|[1-9]\d*)$/.test(value)) return `${value} B`;
  const bytes = BigInt(value);
  const units = [[1024n ** 4n, "TiB"], [1024n ** 3n, "GiB"], [1024n ** 2n, "MiB"], [1024n, "KiB"]] as const;
  for (const [size, label] of units) {
    if (bytes >= size) {
      const tenths = bytes * 10n / size;
      return `${tenths / 10n}.${tenths % 10n} ${label}`;
    }
  }
  return `${bytes} B`;
}

function transferPermille(completed: string, total: string): number {
  const numerator = BigInt(completed);
  const denominator = BigInt(total);
  if (denominator <= 0n) return 0;
  return Number((numerator > denominator ? denominator : numerator) * 1000n / denominator);
}
