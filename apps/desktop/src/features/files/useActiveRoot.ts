import { useCallback, useEffect, useRef } from "react";
import { keyForScope, sameRoot } from "./api";
import { createPaintTicket } from "../../perf/paintTicket";
import { awaitPanePaint } from "../terminal/panePaintGate";
import { useCommittedRef } from "../../commands/useCommittedRef";
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
  /**
   * The working directory the host last reported for the active pane.
   *
   * The one signal that a `cd` happened at all — see the effect that watches it.
   */
  activePaneCurrentPath?: string;
}

/**
 * Resolves and re-checks the pane's active root.
 *
 * Its own hook because none of it is about directories: it owns one request,
 * one supersession rule, and one timer, and the Explorer only needs to be told
 * when the answer changes. Kept inside the directory hook, its four pieces of
 * mutable state sat alongside six others that had nothing to do with them.
 */
export function useActiveRoot(options: Options): {
  /** Something happened that could genuinely have moved the root. Probes now. */
  rearm: () => void;
  /**
   * A person asked for this answer. Probes now, in front of the paint gate.
   *
   * The gate orders the Explorer behind the pane's own screen because on a
   * *switch* the screen is what the user is waiting for and the root probe is
   * speculative. Neither is true of a gesture: pressing Refresh is a request
   * for this answer and nothing else, and making it wait up to
   * `PANE_PAINT_TIMEOUT_MS` for a paint the person did not ask about is the
   * one control they reach for when the Explorer looks stuck taking two thirds
   * of a second to do anything at all.
   */
  rearmNow: () => void;
  /**
   * The user is working, so stop being settled — but issue nothing.
   *
   * The distinction matters because these are the two different facts callers
   * have. Expanding a folder is activity; it is not evidence that the pane's
   * `cd` changed, and treating it as such put a `resolveActiveRoot` — which
   * forks `tmux` on the host — on the Explorer's own interaction path, which is
   * the path this package exists to make cheap.
   */
  noteActivity: () => void;
} {
  const probeSerial = useRef(0);
  const rearmRef = useRef<(() => void) | undefined>(undefined);
  const rearmNowRef = useRef<(() => void) | undefined>(undefined);
  const activityRef = useRef<(() => void) | undefined>(undefined);
  // Read only from the probe, which runs long after the render that set it.
  const latest = useCommittedRef(options);
  const { activePaneCurrentPath, client, scopeKey } = options;
  const lastPanePath = useRef(activePaneCurrentPath);

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

    /**
     * Probes behind the pane's own screen, not ahead of it.
     *
     * Every entry point goes through here, and that is the point. The root
     * cascades into a directory listing *and* a Git watch whose bootstrap is a
     * whole `git status` — 60-80 KB, on the same ordered lane as the answer to
     * the switch — so a probe that skips the gate puts all of it in front of the
     * screen the user is waiting for. Gating only the scope effect was not
     * enough: a switch changes the active pane's `current_path` as well as the
     * scope, and the effect watching that path calls `rearm` *synchronously* in
     * the same commit, taking the `resolving` latch and reducing the gated call
     * to a no-op. That is the ordering the timeline caught.
     *
     * The gate is a timeout and never a barrier: a pane that never paints costs
     * a probe 600 ms and nothing more. It costs nothing at all on the paths it
     * is not there for — a `cd` in the pane the user is already looking at, or
     * the backstop's tick — because a pane that has already painted resolves
     * the wait synchronously.
     *
     * Every entry point *that the user did not ask for*, to be exact. A gesture
     * goes through `rearmNow` and calls `resolve` directly: see its note.
     */
    const resolveBehindPaint = () => {
      const painting = latest.current.scope()?.paneId;
      void (painting === undefined ? Promise.resolve() : awaitPanePaint(painting)).then(resolve);
    };
    resolveBehindPaint();
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
      resolveBehindPaint();
    }, ACTIVE_ROOT_BACKSTOP_MS);
    const noteActivity = () => {
      unchangedProbes = 0;
      ticks = 0;
    };
    const rearm = () => {
      noteActivity();
      if (foreground()) resolveBehindPaint();
    };
    // Ungated on purpose, and only ever reached from an explicit gesture. The
    // `foreground()` check stays: a hidden window's Refresh is not a thing that
    // happens, and the rule that a hidden window issues nothing is worth more
    // than the case it would cover.
    const rearmNow = () => {
      noteActivity();
      if (foreground()) void resolve();
    };
    rearmRef.current = rearm;
    rearmNowRef.current = rearmNow;
    activityRef.current = noteActivity;
    const onVisibility = () => { if (foreground()) rearm(); };
    document?.addEventListener?.("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      probeSerial.current += 1;
      window.clearInterval(backstop);
      document?.removeEventListener?.("visibilitychange", onVisibility);
      rearmRef.current = undefined;
      rearmNowRef.current = undefined;
      activityRef.current = undefined;
    };
  }, [client, scopeKey]);

  /**
   * The pane's working directory moved, so check the root now.
   *
   * tmux announces no `cd` at all, which is why the backstop above exists — but
   * the host does notice: its topology safety reconcile compares each pane's
   * `current_path` every 30 s and pushes a `TopologySnapshot` when one differs,
   * and that snapshot is what changes this value. So the detector already runs
   * on the host, and all this effect does is turn its signal into a single
   * immediate probe rather than leaving it to a timer that has decayed to one
   * check every two minutes. Worst case is the reconcile's own interval — ~30 s
   * for a bare `cd` in an otherwise idle pane — and instant when any tmux
   * activity accompanies it, because that pushes the snapshot straight away.
   *
   * Not on mount (the scope effect above probes immediately) and not across
   * `undefined`, which is connection churn rather than a `cd`. The root itself
   * is the git toplevel, so `cd` within one repository resolves the same root
   * and `rootToken` dedupes the answer to no repaint at all.
   */
  useEffect(() => {
    const previous = lastPanePath.current;
    lastPanePath.current = activePaneCurrentPath;
    if (previous === undefined || activePaneCurrentPath === undefined) return;
    if (previous === activePaneCurrentPath) return;
    rearmRef.current?.();
  }, [activePaneCurrentPath]);

  // Stable, because callers keep it in dependency arrays and in event
  // handlers that must not be rebuilt on every render.
  const rearm = useCallback(() => rearmRef.current?.(), []);
  const rearmNow = useCallback(() => rearmNowRef.current?.(), []);
  const noteActivity = useCallback(() => activityRef.current?.(), []);
  return { rearm, rearmNow, noteActivity };
}
