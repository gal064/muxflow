import { useEffect, useRef } from "react";
import { selectTerminalSession } from "../features/terminal/api";

/** Spacing and count of the retries after a selection the host did not take. */
export const VISIBLE_SESSION_RETRY_MS = 400;
export const VISIBLE_SESSION_RETRIES = 4;

interface VisibleTerminalSessionOptions {
  activeSessionId?: string;
  canMutate: boolean;
  clientId?: string;
  onStatus(message: string): void;
  /**
   * The host's topology generation. Not part of the fact being asserted — it
   * is the clock on which the one refusal this can hit stops being true.
   */
  topologyGeneration: number;
}

/**
 * Keeps the host's idea of the visible workspace equal to this one's.
 *
 * The host attaches one tmux control client per session and takes exactly one
 * of them out of `ignore-size`; that client's windows are the ones
 * `refresh-client -C` moves. Which workspace is on screen is decided entirely
 * on this side, and nothing on the wire carried it: the only message that ever
 * moved the host's answer was the connect-time `AttachTerminal`, aimed at
 * whichever session the fresh snapshot happened to list first. Everything after
 * that — a workspace switch, a reconnect, a snapshot that re-resolved the
 * selection — moved this side alone.
 *
 * So the effect is keyed on the *fact*, not on any of the events that can
 * change it. A new bridge is a new client id and re-asserts; a workspace switch
 * is a new session id and re-asserts; anything else that lands the app on a
 * different session re-asserts for free. That matters more than it sounds:
 * of the paths that change the displayed workspace, only one went through the
 * `SelectSession` tmux action the host reads today, and the reconnect — the
 * quiet one, which happens after every sleep and every network blip — went
 * through none of them.
 *
 * Retried because the failure is transient and known: the host refuses a
 * session whose control client it has not attached yet, and on a fresh bridge
 * its reconciler is still attaching them. The timer is only half of that,
 * though — what actually ends the refusal is a *reconciliation*, so the
 * topology generation is a trigger too. A budget of timed retries alone can
 * expire on a slow link with several sessions and then never try again, which
 * leaves the workspace at tmux's 80x24 default until the user switches away
 * and back: the exact symptom this exists to remove.
 *
 * Reported only once the retries are gone, because a host that will not take
 * the selection is about to size the user's windows from a workspace they
 * cannot see.
 */
export function useVisibleTerminalSession({
  activeSessionId,
  canMutate,
  clientId,
  onStatus,
  topologyGeneration,
}: VisibleTerminalSessionOptions): void {
  /** The fact this hook has already got the host to agree to. */
  const asserted = useRef<string | undefined>(undefined);
  /** The fact this hook has already reported it could not get agreement on. */
  const reported = useRef<string | undefined>(undefined);
  /**
   * Which selection is the current one, and the one before it.
   *
   * Two selections can be outstanding at once — a rapid workspace switch, or a
   * topology generation arriving mid-request — and each is dispatched onto its
   * own blocking task on the other side of the IPC boundary, where they race
   * for the transport. If the earlier one lands last, the host ends up sizing
   * from the workspace the user has just left, and because the later one
   * *resolved* nothing would ever re-send it. So they are chained: the next
   * selection is not issued until the previous has settled, which is what makes
   * "the last one the host saw" and "the last one this asked for" the same
   * message. The counter is the other half — a settled answer from a
   * superseded issue is discarded rather than recorded as the current fact.
   */
  const issue = useRef(0);
  const settled = useRef<Promise<unknown>>(Promise.resolve());
  useEffect(() => {
    if (!clientId || !activeSessionId || !canMutate) return;
    const fact = `${clientId}:${activeSessionId}`;
    // A topology change is a reason to try again, never a reason to re-send
    // something the host already accepted.
    if (asserted.current === fact) return;
    const current = (issue.current += 1);
    const stale = () => issue.current !== current;
    // A fact already reported gets one attempt per topology change rather than
    // a fresh budget of five, and says nothing further. What this retries for
    // is transient by construction — a control client the reconciler has not
    // attached yet — so a refusal that outlives its budget is most likely
    // permanent, and an old helper that does not know the operation would
    // otherwise cost five round trips and a toast on every window rename.
    const budget = reported.current === fact ? 0 : VISIBLE_SESSION_RETRIES;
    let attempt = 0;
    let timer = 0;
    const assert = () => {
      const request = settled.current
        .catch(() => undefined)
        .then(() => (stale() ? undefined : selectTerminalSession(clientId, activeSessionId)));
      settled.current = request.catch(() => undefined);
      void request.then(() => {
        if (!stale()) asserted.current = fact;
      }).catch((error) => {
        if (stale()) return;
        if (attempt < budget) {
          attempt += 1;
          timer = window.setTimeout(assert, VISIBLE_SESSION_RETRY_MS);
          return;
        }
        if (reported.current === fact) return;
        reported.current = fact;
        onStatus(`This workspace may render at the wrong size: ${String(error)}`);
      });
    };
    assert();
    // Only the timer is cancelled here. The issue counter is bumped by the
    // *next* run, so that an unmount with nothing following it does not make
    // an in-flight answer look superseded.
    return () => window.clearTimeout(timer);
    // `onStatus` is deliberately not a dependency: it is re-created on most
    // renders, and re-running this would re-send the selection for nothing.
  }, [activeSessionId, canMutate, clientId, topologyGeneration]);
}
