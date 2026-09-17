import type { Window as TmuxWindow } from "./types";
import type { TmuxAction } from "../features/tmux/actions";

export type WindowMoveDirection = "left" | "right";

export function relativeWindowReorderAction(
  windows: readonly TmuxWindow[],
  activeWindowId: string,
  direction: WindowMoveDirection,
): TmuxAction | undefined {
  const ordered = [...windows].sort((left, right) => left.index - right.index);
  const current = ordered.findIndex((window) => window.id === activeWindowId);
  const target = ordered[current + (direction === "left" ? -1 : 1)];
  const active = ordered[current];
  if (!active || !target || target.sessionId !== active.sessionId || target.id === active.id) return undefined;
  return {
    kind: "reorderWindow",
    sessionId: active.sessionId,
    windowId: active.id,
    targetWindowId: target.id,
    relativePosition: direction === "left" ? "before" : "after",
  };
}

/**
 * Which window the shell should be showing.
 *
 * The host's own active window wins by default: tmux is authoritative, another
 * client may have moved it, and the shell following it is the whole point.
 *
 * `optimistic` is the one exception, and it exists because that default is
 * also what reverts an optimistic switch. A switch committed locally is ahead
 * of the host by design — the select-window request is still in flight — so
 * every snapshot in that gap still names the *old* window as active and would
 * snap the UI back to it. While a switch is outstanding the target wins, and
 * only until the guard is released; see `optimisticWindowSwitch` in
 * useShellNavigation, which releases it once a snapshot has caught up.
 *
 * It must still be a window that exists: a target that has since been closed
 * is not somewhere the shell can sit, and preferring it would strand the UI on
 * nothing.
 */
export function resolveActiveWindowId(
  windows: readonly TmuxWindow[],
  current: string | undefined,
  optimistic?: string,
): string | undefined {
  if (optimistic && windows.some((window) => window.id === optimistic)) return optimistic;
  return windows.find((window) => window.active)?.id
    ?? windows.find((window) => window.id === current)?.id
    ?? windows[0]?.id;
}

/**
 * Which window a workspace should be showing, from an unfiltered snapshot.
 *
 * The same rule as `resolveActiveWindowId`, applied to one session's windows
 * without waiting for the shell's filtered view of them to exist. A workspace
 * switch needs the answer in the same tick it commits the workspace: resolving
 * it a paint later is what shows the new workspace against the old
 * workspace's window for one frame.
 *
 * Sorted by index for the same reason the shell's window list is: the
 * last-resort fallback is "the first window", and first has to mean the same
 * thing in both places.
 */
export function windowForSession(
  windows: readonly TmuxWindow[],
  sessionId: string,
  previous?: string,
): string | undefined {
  const owned = windows
    .filter((window) => window.sessionId === sessionId)
    .sort((left, right) => left.index - right.index);
  return resolveActiveWindowId(owned, previous);
}

/**
 * A switch committed locally while its request is still in flight.
 *
 * The shape the snap-back guard needs: which switch it is, and the generation
 * at which the host is known to have seen it. `throughGeneration` is unset
 * until the select action answers — before that there is no generation to wait
 * for, and the guard simply holds.
 *
 * Covers a workspace switch as well as a window one, because both decide the
 * same thing — which window the shell shows — and a user can only be making
 * one of them at a time. `windowId` is unset where a workspace switch could
 * not name its window yet: a workspace never visited under this connection has
 * no windows in the snapshot, so there is nothing to hold and the guard only
 * marks the switch as outstanding.
 */
export interface OptimisticWindowSwitch {
  sessionId: string;
  windowId?: string;
  throughGeneration?: number;
}
