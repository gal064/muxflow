/**
 * Where the user has been, so Back and Forward — the titlebar arrows and
 * ⌘[ / ⌘] — can take them back.
 *
 * This is the model behind them: a bounded, linear history of tabs with a
 * cursor, the browser model rather than a stack, because "back then forward"
 * has to land where it started. A point names a terminal (workspace and
 * window) or a document tab (workspace, the window it was opened over, and
 * the tab), so the history walks through files, Markdown and Git diffs the
 * same way it walks through terminals.
 */
export interface FocusPoint {
  sessionId: string;
  windowId?: string;
  /** Set for a document tab; absent for a terminal. */
  appTabId?: string;
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
  return left?.sessionId === right?.sessionId && left?.windowId === right?.windowId
    && left?.appTabId === right?.appTabId;
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
 * The nearest entry in `direction` that still exists — the walk skips points
 * whose tab or workspace has gone, rather than stopping on them, so a closed
 * tab between here and the destination is stepped over, not navigated to.
 */
export function stepFocusToValid(
  history: FocusHistory,
  direction: "back" | "forward",
  exists: (point: FocusPoint) => boolean,
): { history: FocusHistory; point?: FocusPoint } {
  let current = history;
  for (;;) {
    const stepped = stepFocus(current, direction);
    if (!stepped.point) return { history };
    current = stepped.history;
    if (exists(stepped.point)) return { history: current, point: stepped.point };
  }
}

/** Whether `stepFocusToValid` would find somewhere to go. */
export function hasValidStep(
  history: FocusHistory,
  direction: "back" | "forward",
  exists: (point: FocusPoint) => boolean,
): boolean {
  return stepFocusToValid(history, direction, exists).point !== undefined;
}

/**
 * The entry a closing tab should hand focus to: the nearest one before the
 * cursor that exists and is not `exclude` — the tab being closed is still in
 * the state when this is asked, and must not be its own destination.
 */
export function nearestValidBack(
  history: FocusHistory,
  exists: (point: FocusPoint) => boolean,
  exclude?: Pick<FocusPoint, "appTabId">,
): { index: number; point: FocusPoint } | undefined {
  for (let index = history.cursor - 1; index >= 0; index -= 1) {
    const point = history.entries[index];
    if (exclude?.appTabId !== undefined && point.appTabId === exclude.appTabId) continue;
    if (exists(point)) return { index, point };
  }
  return undefined;
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
