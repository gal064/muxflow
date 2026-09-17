import { useId } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import { SurfaceError } from "../../ui/SurfaceError";
import type { AgentHookReview } from "./types";

interface HookReviewDialogProps {
  review: AgentHookReview;
  applying: boolean;
  error?: string;
  onCancel(): void;
  onConfirm(): void;
}

export function HookReviewDialog(props: HookReviewDialogProps) {
  const adapter = props.review.adapterDisplayName ?? props.review.adapterId;
  const installing = props.review.action === "install";
  const titleId = useId();
  const detailId = useId();
  const dialog = useModalDialog<HTMLElement>(() => { if (!props.applying) props.onCancel(); });
  return <div className="modal-backdrop hook-review-backdrop" role="presentation">
    <section aria-describedby={detailId} aria-labelledby={titleId} aria-modal="true" className="modal hook-review" ref={dialog} role="alertdialog">
      <header>
        <div><small>Review before changing config</small><h2 id={titleId}>{installing ? "Install" : "Uninstall"} {adapter} hooks?</h2></div>
      </header>
      <div className="hook-review-scroll">
        <p id={detailId}>Review the exact host-proposed configuration changes. Unrelated configuration must remain untouched. Plan label: <code>{props.review.managedLabel}</code>.</p>
        <ul>
          {props.review.changes.map((change) => <li key={change.path}>
            <strong>{change.path}</strong><span>{change.summary}</span>
            <dl><dt>Ownership marker</dt><dd><code>{change.owner || "No ownership marker"}</code></dd><dt>Command</dt><dd><code>{change.command || "No command supplied"}</code></dd><dt>Events</dt><dd>{change.events.length ? change.events.join(", ") : "No events supplied"}</dd><dt>Operation</dt><dd>{change.createsConfig ? "Create config" : change.removesConfig ? "Remove config" : "Update config"}</dd><dt>Before hash</dt><dd><code>{change.beforeHash || "none"}</code></dd><dt>After hash</dt><dd><code>{change.afterHash || "none"}</code></dd></dl>
            {change.diffPreview && <><h3>Redacted configuration diff</h3><pre aria-label={`Redacted configuration diff for ${change.path}`}>{change.diffPreview}</pre></>}
            {!change.diffPreview && (change.beforePreview || change.afterPreview) && <div className="hook-preview-pair"><section><h3>Before · redacted</h3><pre>{change.beforePreview || "(empty)"}</pre></section><section><h3>After · redacted</h3><pre>{change.afterPreview || "(empty)"}</pre></section></div>}
            {change.previewTruncated && <p role="note">Preview truncated at the host’s bounded redaction limit; hashes identify the complete configurations.</p>}
          </li>)}
        </ul>
        {props.review.trustGuidance && <p className="hook-trust" role="note">{props.review.trustGuidance}</p>}
        {props.review.backupPath && <p className="hook-backup">Backup: <code>{props.review.backupPath}</code></p>}
        {props.review.alreadyInstalled && <p role="status">The managed hook is already current. Confirming is idempotent.</p>}
        {props.error && <SurfaceError className="dialog-error" detail={props.error} />}
      </div>
      <footer>
        <button disabled={props.applying} onClick={props.onCancel} type="button">Cancel</button>
        <button className={installing ? "primary" : "danger"} disabled={props.applying} onClick={props.onConfirm} type="button">{props.applying ? "Applying…" : `${installing ? "Install" : "Uninstall"} reviewed hooks`}</button>
      </footer>
    </section>
  </div>;
}
