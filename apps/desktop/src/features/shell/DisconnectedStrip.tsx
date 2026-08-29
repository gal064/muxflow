import { useEffect, useState } from "react";
import type { ConnectionPhase } from "../../state/connectionReducer";

/**
 * How long the link must stay degraded before the strip is worth showing.
 *
 * Sub-400ms blips are self-healing repairs whose banner would only be legible
 * as a flash — the native link dips through `resyncing` for the length of one
 * round trip and comes back with the stream intact.
 */
export const STRIP_APPEAR_DELAY_MS = 400;

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
 * It is deliberately a single fixed-height row that ellipsizes, and it overlays
 * the shell body rather than taking a row from it. The tmux client size is
 * measured from the terminal surface directly below, so a strip that appeared
 * *in flow* resized every one of the user's real tmux windows on the way in and
 * again on the way out — churn charged to a banner that is often gone within a
 * second. The full text stays on the element's title and in the live region.
 *
 * It also waits: see `STRIP_APPEAR_DELAY_MS`. Recovery is not delayed — the
 * strip leaves the moment the phase is healthy again.
 */
export function DisconnectedStrip(props: DisconnectedStripProps) {
  const degraded = props.phase !== "connected";
  const [settled, setSettled] = useState(false);
  useEffect(() => {
    if (!degraded) {
      setSettled(false);
      return;
    }
    const timer = setTimeout(() => setSettled(true), STRIP_APPEAR_DELAY_MS);
    return () => clearTimeout(timer);
  }, [degraded]);
  if (!degraded || !settled) return null;
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
