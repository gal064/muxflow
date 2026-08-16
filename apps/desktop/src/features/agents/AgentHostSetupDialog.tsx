import { useId } from "react";
import { useModalDialog } from "../../commands/useModalDialog";
import { SurfaceError } from "../../ui/SurfaceError";
import type { AgentAdapterDescriptor } from "./types";

interface AgentHostSetupDialogProps {
  hostLabel: string;
  adapters: readonly AgentAdapterDescriptor[];
  activity?: "install" | "review";
  error?: string;
  onDecline(): void;
  onAccept(): void;
  onReview(): void;
}

/**
 * The one-time "set up this host" prompt.
 *
 * It exists because the alternative — waiting for the user to find a hook
 * installer in a context menu — is what left the field machine reporting
 * nothing for an entire phase. It is a prompt rather than a silent install
 * because it changes files the user's other tools also write, and the exact
 * change is one click away rather than behind a decision to trust.
 *
 * Deliberately not a checklist of adapters: the user is answering about a host,
 * not choosing between two agent vendors they may not distinguish. Which
 * configuration files are involved is stated, because that is the part that
 * would make someone say no.
 */
export function AgentHostSetupDialog(props: AgentHostSetupDialogProps) {
  const titleId = useId();
  const detailId = useId();
  const busy = props.activity !== undefined;
  const dialog = useModalDialog<HTMLElement>(() => { if (!busy) props.onDecline(); });
  return <div className="modal-backdrop host-setup-backdrop" role="presentation">
    <section aria-describedby={detailId} aria-labelledby={titleId} aria-modal="true" className="host-setup" ref={dialog} role="alertdialog">
      <header>
        <div><small>One-time setup</small><h2 id={titleId}>Set up agent status on {props.hostLabel}?</h2></div>
      </header>
      <div className="host-setup-body">
        <p id={detailId}>
          Agents on this host cannot report what they are doing yet. Setting up adds this
          app’s lifecycle hooks alongside whatever is already configured — nothing existing
          is replaced, and a timestamped backup is written before the first change.
        </p>
        <ul className="host-setup-paths">
          {props.adapters.map((adapter) => <li key={adapter.id}>
            <strong>{adapter.displayName}</strong>
            <code>{adapter.hookConfigPath}</code>
          </li>)}
        </ul>
        <p>
          It also asks this host’s tmux server to name agent windows after what the
          agent is working on. Only agent windows — everything else keeps the name
          tmux gives it — and nothing is written to your tmux config.
        </p>
        <p className="quiet-note">Until then the agents list stays honest: it shows which agents exist and says nothing about what they are doing.</p>
        {props.error && <SurfaceError className="dialog-error" detail={props.error} />}
      </div>
      <footer>
        <button disabled={busy} onClick={props.onDecline} type="button">Not now</button>
        <button disabled={busy} onClick={props.onReview} type="button">{props.activity === "review" ? "Loading review…" : "Review exact changes…"}</button>
        <button className="primary" disabled={busy} onClick={props.onAccept} type="button">
          {props.activity === "install" ? "Setting up…" : "Set up this host"}
        </button>
      </footer>
    </section>
  </div>;
}
