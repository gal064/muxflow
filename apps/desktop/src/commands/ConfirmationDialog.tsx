import { useId } from "react";
import { useModalDialog } from "./useModalDialog";

interface Props {
  detail: string;
  title: string;
  confirmLabel?: string;
  onCancel(): void;
  onConfirm(): void;
}

export function ConfirmationDialog({ confirmLabel = "Close permanently", detail, title, onCancel, onConfirm }: Props) {
  const titleId = useId();
  const detailId = useId();
  const dialog = useModalDialog<HTMLElement>(onCancel);
  return <div className="modal-backdrop" role="presentation">
    <section aria-describedby={detailId} aria-labelledby={titleId} aria-modal="true" className="confirmation" ref={dialog} role="alertdialog">
      <h2 id={titleId}>{title}</h2>
      <p id={detailId}>{detail}</p>
      <div className="dialog-actions">
        <button autoFocus onClick={onCancel} type="button">Cancel</button>
        <button className={confirmLabel.startsWith("Close") ? "danger" : "primary"} onClick={onConfirm} type="button">{confirmLabel}</button>
      </div>
    </section>
  </div>;
}
