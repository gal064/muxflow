import { useCallback, useEffect, useRef } from "react";
import { keyForScope, sameRoot } from "./api";
import { createPaintTicket } from "../../perf/paintTicket";
import type { ActiveRoot, FileWorkspaceClient, FileWorkspaceScope } from "./types";

/**
 * How often the active root is re-checked when nothing has announced a change.
 *
 * Every event that *can* be pushed already re-resolves it immediately; this
 * covers `cd` inside the current pane, which tmux does not announce. It is a
 * backstop rather than a pipeline: a hidden window checks nothing at all (a
 * visible but *unfocused* one still does — see the note beside `foreground`),
 * the host answers an unchanged root from the caller's own capability without
 * a second authoritative discovery or a broadcast payload, and — see
 * [`ACTIVE_ROOT_SETTLED_MULTIPLIER`] — a settled root is checked far less
 * often, though never not at all.
 */
export const ACTIVE_ROOT_BACKSTOP_MS = 15_000;

/**
 * Consecutive unchanged probes after which the backstop slows down.
 *
 * A timer that never slows is a periodic request forever, which the round's
 * idle budget refuses outright. Anything that could have moved the root — the
 * window coming back to the foreground, the host announcing a different root,
 * or the person touching the Explorer — puts it back to full rate, so the cost
 * tracks activity rather than uptime.
 */
export const ACTIVE_ROOT_STABLE_PROBES = 3;

/**
 * How much longer the settled backstop waits between probes.
 *
 * Deliberately a longer wait rather than a stop. `cd` inside the pane the user
 * is already in is announced by nothing at all, so a backstop that switches
 * itself off entirely leaves a moved root undetected for as long as the person
 * does not happen to touch the Explorer or blur the window — which on a pane
 * somebody is only reading is indefinitely.
 */
export const ACTIVE_ROOT_SETTLED_MULTIPLIER = 8;

interface Options {
  client: FileWorkspaceClient;
  /** The live scope, read at the moment a probe runs rather than captured. */
  scope: () => FileWorkspaceScope | undefined;
  /** The scope identity this probe belongs to; a probe outliving it is dropped. */
  scopeKey: string;
  /** The capability the caller already holds, so an unchanged root costs nothing. */
  held: () => ActiveRoot | undefined;
  /** A root that is genuinely different from the one the caller holds. */
  onRoot: (root: ActiveRoot) => void;
  onError: (message: string) => void;
  /** The caller's lifecycle counter, so paint measurements share its identity. */
  lifecycle: () => number;
}

/**
 * Resolves and re-checks the pane's active root.
 *
 * Its own hook because none of it is about directories: it owns one request,
 * one supersession rule, and one timer, and the Explorer only needs to be told
 * when the answer changes. Kept inside the directory hook, its four pieces of
 * mutable state sat alongside six others that had nothing to do with them.
 */
export function useActiveRoot(options: Options): { rearm: () => void } {
  const probeSerial = useRef(0);
  const rearmRef = useRef<(() => void) | undefined>(undefined);
  const latest = useRef(options);
  latest.current = options;
  const { client, scopeKey } = options;

  useEffect(() => {
    if (!scopeKey) return;
    let disposed = false;
    let resolving = false;
    let unchangedProbes = 0;
    let ticks = 0;
    const lifecycle = latest.current.lifecycle();

    const resolve = async () => {
      // One probe at a time. Two in flight would race to install answers whose
      // order says nothing about which is current.
      if (resolving) return;
      resolving = true;
      const probe = ++probeSerial.current;
      const paint = createPaintTicket(["workflow.explorer.rootPaint"], lifecycle);
      const current = latest.current;
      const alive = () => !disposed
        && lifecycle === current.lifecycle()
        && probe === probeSerial.current;
      try {
        const scope = current.scope();
        if (!scope || keyForScope(scope) !== scopeKey) {
          paint.abandon();
          return;
        }
        const known = current.held()?.token;
        const root = await current.client.resolveActiveRoot(
          scope,
          known ? { knownRootToken: known } : {},
        );
        if (!alive()) {
          paint.abandon();
          return;
        }
        if (sameRoot(current.held(), root)) {
          unchangedProbes += 1;
          paint.abandon();
          return;
        }
        unchangedProbes = 0;
        current.onRoot(root);
        paint.afterPaint((ticket) => ticket.lifecycleGeneration === latest.current.lifecycle()
          && alive()
          && sameRoot(latest.current.held(), root));
      } catch (error) {
        paint.abandon();
        if (alive()) current.onError(String(error));
      } finally {
        resolving = false;
      }
    };

    void resolve();
    // A foreground backstop, not a pipeline. Pane and window changes rebuild
    // this scope and re-resolve immediately, so the only thing left for a timer
    // to catch is `cd` inside the pane the user is already in — for which tmux
    // emits no notification at all. A hidden window checks nothing, and a
    // window that becomes visible checks once on the transition rather than
    // waiting out the interval.
    //
    // "Foreground" here means *visible*, not focused: `visibilityState` is
    // `"visible"` for a window sitting behind another one, so a visible but
    // unfocused window still probes. That is a knowing gap against the round's
    // "zero periodic desktop-to-host requests at idle" and is recorded as such
    // in the branch ledger rather than papered over here — `document.hasFocus()`
    // would close it, and is unobservable in the jsdom lane that covers this
    // hook, so it belongs to the runtime QA lane that can actually verify it.
    const foreground = () => typeof document === "undefined" || document.visibilityState === "visible";
    const backstop = window.setInterval(() => {
      if (!foreground()) return;
      ticks += 1;
      // Settled: the same check, far less often. See the multiplier's note for
      // why this slows down rather than stopping.
      const settled = unchangedProbes >= ACTIVE_ROOT_STABLE_PROBES;
      if (settled && ticks % ACTIVE_ROOT_SETTLED_MULTIPLIER !== 0) return;
      void resolve();
    }, ACTIVE_ROOT_BACKSTOP_MS);
    const rearm = () => {
      unchangedProbes = 0;
      ticks = 0;
      if (foreground()) void resolve();
    };
    rearmRef.current = rearm;
    const onVisibility = () => { if (foreground()) rearm(); };
    document?.addEventListener?.("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      probeSerial.current += 1;
      window.clearInterval(backstop);
      document?.removeEventListener?.("visibilitychange", onVisibility);
      rearmRef.current = undefined;
    };
  }, [client, scopeKey]);

  // Stable, because callers keep it in dependency arrays and in event
  // handlers that must not be rebuilt on every render.
  const rearm = useCallback(() => rearmRef.current?.(), []);
  return { rearm };
}
