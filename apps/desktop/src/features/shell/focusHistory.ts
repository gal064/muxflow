/**
 * Where the user has been, so ⌘[ and ⌘] can take them back.
 *
 * The redesigned titlebar carries four controls and no more, so the mock's
 * back/forward arrows do not survive as resting chrome — but the capability
 * they stood for does, as two commands. This is the model behind them: a
 * bounded, linear history of (workspace, tab) pairs with a cursor, the browser
 * model rather than a stack, because "back then forward" has to land where it
 * started.
 */
export interface FocusPoint {
  sessionId: string;
  windowId?: string;
}

export interface FocusHistory {
  entries: readonly FocusPoint[];
  /** Index of the entry currently showing; -1 when the history is empty. */
  cursor: number;
}

/** Deep enough to be useful, shallow enough that "back" stays predictable. */
export const FOCUS_HISTORY_LIMIT = 50;

export const emptyFocusHistory: FocusHistory = { entries: [], cursor: -1 };

export function samePoint(left: FocusPoint | undefined, right: FocusPoint | undefined): boolean {
  return left?.sessionId === right?.sessionId && left?.windowId === right?.windowId;
}

/**
 * Records arriving somewhere. Arriving where you already are is not a move, and
 * arriving anywhere after going back truncates the forward branch — the same
 * rule a browser uses.
 */
export function visitFocus(history: FocusHistory, point: FocusPoint): FocusHistory {
  if (samePoint(history.entries[history.cursor], point)) return history;
  const kept = history.entries.slice(0, history.cursor + 1);
  const entries = [...kept, point].slice(-FOCUS_HISTORY_LIMIT);
  return { entries, cursor: entries.length - 1 };
}

export function canGoBack(history: FocusHistory): boolean {
  return history.cursor > 0;
}

export function canGoForward(history: FocusHistory): boolean {
  return history.cursor >= 0 && history.cursor < history.entries.length - 1;
}

export function stepFocus(history: FocusHistory, direction: "back" | "forward"): {
  history: FocusHistory;
  point?: FocusPoint;
} {
  const next = history.cursor + (direction === "back" ? -1 : 1);
  if (next < 0 || next >= history.entries.length) return { history };
  return { history: { ...history, cursor: next }, point: history.entries[next] };
}

/**
 * Drops points that no longer exist. A tmux window can be closed from another
 * client at any moment, and "back" must not navigate to something gone.
 */
export function pruneFocusHistory(
  history: FocusHistory,
  exists: (point: FocusPoint) => boolean,
): FocusHistory {
  const current = history.entries[history.cursor];
  const entries = history.entries.filter(exists);
  if (entries.length === history.entries.length) return history;
  const cursor = current && entries.includes(current)
    ? entries.indexOf(current)
    : entries.length - 1;
  return { entries, cursor };
}
