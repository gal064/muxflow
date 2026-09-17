import { useId, useRef, useState } from "react";
import { useModalDialog } from "./useModalDialog";

export interface PendingTextPrompt {
  title: string;
  label: string;
  initialValue?: string;
  submit(value: string): void;
}

interface Props extends PendingTextPrompt {
  onCancel(): void;
}

export function TextInputDialog({ initialValue = "", label, onCancel, submit, title }: Props) {
  const [value, setValue] = useState(initialValue);
  const titleId = useId();
  const composing = useRef(false);
  const dialog = useModalDialog<HTMLElement>(onCancel);
  return <div className="modal-backdrop" role="presentation">
    <section aria-labelledby={titleId} aria-modal="true" className="confirmation text-input-dialog" ref={dialog} role="dialog">
      <h2 id={titleId}>{title}</h2>
      <form onSubmit={(event) => {
        event.preventDefault();
        if (composing.current) return;
        const trimmed = value.trim();
        if (trimmed) submit(trimmed);
      }}>
        <label>{label}<input autoFocus onChange={(event) => setValue(event.target.value)} onCompositionEnd={() => { composing.current = false; }} onCompositionStart={() => { composing.current = true; }} value={value} /></label>
        <div className="dialog-actions"><button onClick={onCancel} type="button">Cancel</button><button className="primary" disabled={!value.trim()} type="submit">Save</button></div>
      </form>
    </section>
  </div>;
}
