import { useId } from "react";
import { useModalDialog } from "./useModalDialog";

interface Props {
  detail: string;
  title: string;
  confirmLabel?: string;
  /**
   * Whether the confirm button is drawn as destructive.
   *
   * Stated, not inferred. It used to be read off the *label* —
   * `confirmLabel.startsWith("Close")` — so a dialog whose button said "Delete
   * host" got the accent-blue treatment reserved for safe primary actions,
   * identical to "Connect". A one-bit fact about what a button does should not
   * be encoded in the first word of its text. Defaulted from the old rule so
   * the existing "Close permanently" callers are unchanged.
   */
  destructive?: boolean;
  onCancel(): void;
  onConfirm(): void;
}

export function ConfirmationDialog({
  confirmLabel = "Close permanently",
  destructive = confirmLabel.startsWith("Close"),
  detail,
  title,
  onCancel,
  onConfirm,
}: Props) {
  const titleId = useId();
  const detailId = useId();
  const dialog = useModalDialog<HTMLElement>(onCancel);
  return <div className="modal-backdrop" role="presentation">
    <section aria-describedby={detailId} aria-labelledby={titleId} aria-modal="true" className="confirmation" ref={dialog} role="alertdialog">
      <h2 id={titleId}>{title}</h2>
      <p id={detailId}>{detail}</p>
      <div className="dialog-actions">
        <button autoFocus onClick={onCancel} type="button">Cancel</button>
        <button className={destructive ? "danger" : "primary"} onClick={onConfirm} type="button">{confirmLabel}</button>
      </div>
    </section>
  </div>;
}
