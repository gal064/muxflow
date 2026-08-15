import { useEffect } from "react";
import { selectTerminalSession } from "../features/terminal/api";

/** Spacing and count of the retries after a selection the host did not take. */
export const VISIBLE_SESSION_RETRY_MS = 400;
export const VISIBLE_SESSION_RETRIES = 4;

interface VisibleTerminalSessionOptions {
  activeSessionId?: string;
  canMutate: boolean;
  clientId?: string;
  onStatus(message: string): void;
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
 * session that has no control client yet, and on a fresh bridge its reconciler
 * is still attaching them. Reported only once the retries are gone, because a
 * host that will not take the selection is a host that is about to size the
 * user's windows from a workspace they cannot see.
 */
export function useVisibleTerminalSession({
  activeSessionId,
  canMutate,
  clientId,
  onStatus,
}: VisibleTerminalSessionOptions): void {
  useEffect(() => {
    if (!clientId || !activeSessionId || !canMutate) return;
    let cancelled = false;
    let attempt = 0;
    let timer = 0;
    const assert = () => {
      void selectTerminalSession(clientId, activeSessionId).catch((error) => {
        if (cancelled) return;
        if (attempt < VISIBLE_SESSION_RETRIES) {
          attempt += 1;
          timer = window.setTimeout(assert, VISIBLE_SESSION_RETRY_MS);
          return;
        }
        onStatus(`This workspace may render at the wrong size: ${String(error)}`);
      });
    };
    assert();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // `onStatus` is deliberately not a dependency: it is re-created on most
    // renders, and re-running this would re-send the selection for nothing.
  }, [activeSessionId, canMutate, clientId]);
}
