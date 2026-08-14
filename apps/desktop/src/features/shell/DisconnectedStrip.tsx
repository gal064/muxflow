import type { ConnectionPhase } from "../../state/connectionReducer";

interface DisconnectedStripProps {
  phase: ConnectionPhase;
  detail: string;
  hasSnapshot: boolean;
  onReconnect(): void;
  onOpenSettings(): void;
}

const TITLES: Partial<Record<ConnectionPhase, string>> = {
  connecting: "Connecting to tmux…",
  reconnecting: "Reconnecting to tmux…",
  resyncing: "Reconciling authoritative state…",
  disconnected: "Disconnected from tmux",
  readOnly: "Connected read-only",
};

/**
 * The only thing that appears when the connection is not healthy.
 *
 * Before Phase 11 the app reported connection state in three places at once —
 * a permanent "Live" banner, an active-pane block, and a floating badge over
 * the terminal. Two of those are gone and the third is this: nothing at all
 * while connected, one line while not.
 *
 * It is deliberately a single fixed-height row that ellipsizes. This strip sits
 * directly above the terminal surface, and the tmux client size is measured
 * from that surface — a status line that grows by a wrapped row would shrink
 * the user's real tmux windows by a row, and the resulting topology push would
 * re-render the line and do it again. The full text stays on the element's
 * title and in the shell's live region.
 */
export function DisconnectedStrip(props: DisconnectedStripProps) {
  if (props.phase === "connected") return null;
  const readOnly = props.phase === "readOnly";
  const title = TITLES[props.phase] ?? "Disconnected from tmux";
  const detail = props.detail
    || (props.hasSnapshot
      ? "The last known workspace stays visible; writes are frozen and are not queued."
      : "Workspace data appears after a complete authoritative snapshot.");
  return <div className={`link-strip ${readOnly ? "link-strip-frozen" : ""}`}>
    {/* The live region wraps the *words* and not the buttons. With the buttons
        inside it, every change of `detail` re-announced "Reconnect" and
        "Connection…" along with it.
        `status`/`polite` even for read-only: it is a persistent condition, and
        an assertive region re-interrupts on every re-render. The words say
        "read-only" and forced-colors appends it too. */}
    <span aria-live="polite" className="link-strip-message" role="status">
      <span className="link-strip-title">{title}</span>
      <span className="link-strip-detail" title={detail}>{detail}</span>
    </span>
    {!readOnly && <button className="link-strip-action" onClick={props.onReconnect} type="button">Reconnect</button>}
    <button className="link-strip-action" onClick={props.onOpenSettings} type="button">Connection…</button>
  </div>;
}
