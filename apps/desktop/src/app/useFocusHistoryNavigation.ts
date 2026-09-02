import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Session, Window as TmuxWindow } from "./types";
import type { AppOwnedTab } from "../features/shell/types";
import {
  emptyFocusHistory,
  hasValidStep,
  nearestValidBack,
  pruneFocusHistory,
  stepFocusToValid,
  visitFocus,
  type FocusHistory,
  type FocusPoint,
} from "../features/shell/focusHistory";

export interface FocusHistoryNavigationOptions {
  /** The host whose sessions and windows the points describe. */
  hostProfileId: string;
  activeSessionId?: string;
  activeWindowId?: string;
  selectedAppTabId?: string;
  sessions: readonly Session[];
  windows: readonly TmuxWindow[];
  appTabs: readonly AppOwnedTab[];
  selectSession(sessionId: string): void;
  selectWindow(windowId: string): void;
  selectAppTab(sessionId: string, windowId: string | undefined, appTabId: string): void;
  /** Uncovers the terminal under a selected document tab, without moving the window. */
  revealTerminal(sessionId: string, windowId: string | undefined): void;
  setStatus(status: string): void;
}

/**
 * Whether the app landed where a traversal meant to take it. A document tab
 * is identified by its id — the window recorded under it is where it was
 * opened, which need not be the window under it now — while a terminal is
 * identified by its window.
 */
function landedAt(expected: FocusPoint, actual: FocusPoint): boolean {
  if (expected.sessionId !== actual.sessionId || expected.appTabId !== actual.appTabId) return false;
  return Boolean(expected.appTabId) || expected.windowId === actual.windowId;
}

/**
 * The focus history and the two ways through it: Back/Forward, and the step
 * a closing document takes to whatever was showing before it.
 *
 * Recording follows where the app actually ended up, whatever moved it — a
 * click, a shortcut, an agent notification, or tmux itself. Except when a
 * traversal moved it: that is a walk through the history, not a new
 * destination, and recording it would truncate the branch being walked. The
 * traversal therefore names the point it expects to land on, and the record
 * effect skips exactly that arrival — and records normally if the navigation
 * landed somewhere else.
 */
export function useFocusHistoryNavigation(options: FocusHistoryNavigationOptions) {
  const [history, setHistory] = useState<FocusHistory>(emptyFocusHistory);
  const historyRef = useRef(history);
  historyRef.current = history;
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const expected = useRef<FocusPoint | undefined>(undefined);
  const { activeSessionId, activeWindowId, hostProfileId, selectedAppTabId, sessions, windows, appTabs } = options;

  const exists = useCallback((point: FocusPoint) => sessions.some((session) => session.id === point.sessionId)
    && (!point.windowId || windows.some((window) => window.id === point.windowId))
    && (!point.appTabId || appTabs.some((tab) => tab.id === point.appTabId)), [appTabs, sessions, windows]);
  const existsRef = useRef(exists);
  existsRef.current = exists;

  // A different host is a different tmux server with its own `$0` and
  // `@0`: a point recorded on one must not be "restored" on the other. Cleared
  // before the visit below records the new host's first point.
  useEffect(() => { setHistory(emptyFocusHistory); }, [hostProfileId]);

  useEffect(() => {
    if (!activeSessionId) return;
    const point: FocusPoint = { sessionId: activeSessionId, windowId: activeWindowId, appTabId: selectedAppTabId };
    const awaited = expected.current;
    expected.current = undefined;
    if (awaited && landedAt(awaited, point)) return;
    setHistory((current) => visitFocus(current, point));
  }, [activeSessionId, activeWindowId, selectedAppTabId]);

  useEffect(() => {
    setHistory((current) => pruneFocusHistory(current, exists));
  }, [exists]);

  const navigateTo = useCallback((point: FocusPoint) => {
    const current = optionsRef.current;
    expected.current = point;
    if (point.appTabId) {
      if (point.sessionId !== current.activeSessionId) current.selectSession(point.sessionId);
      current.selectAppTab(point.sessionId, point.windowId ?? current.activeWindowId, point.appTabId);
    } else if (point.sessionId !== current.activeSessionId) current.selectSession(point.sessionId);
    else if (point.windowId && point.windowId !== current.activeWindowId) current.selectWindow(point.windowId);
    else if (current.selectedAppTabId) current.revealTerminal(point.sessionId, point.windowId ?? current.activeWindowId);
    else expected.current = undefined;
  }, []);

  // Navigation happens here, not inside a state updater. React invokes
  // updaters twice under StrictMode, and an updater that dispatched tmux
  // actions therefore sent each one twice.
  const step = useCallback((direction: "back" | "forward") => {
    const stepped = stepFocusToValid(historyRef.current, direction, existsRef.current);
    if (!stepped.point) {
      optionsRef.current.setStatus(direction === "back" ? "Nothing earlier to go back to." : "Nothing later to go forward to.");
      return;
    }
    setHistory(stepped.history);
    navigateTo(stepped.point);
  }, [navigateTo]);

  /**
   * Moves to the nearest earlier entry that is not the tab about to close.
   * A traversal, not a visit: the cursor moves to that entry, and the closed
   * tab's own entries are pruned once the close lands. Returns false when
   * there is nothing earlier, and the caller falls back on its own rule.
   */
  const navigateBackFromClosing = useCallback((appTabId: string): boolean => {
    const found = nearestValidBack(historyRef.current, existsRef.current, { appTabId });
    if (!found) return false;
    setHistory({ ...historyRef.current, cursor: found.index });
    navigateTo(found.point);
    return true;
  }, [navigateTo]);

  const canGoBack = useMemo(() => hasValidStep(history, "back", exists), [exists, history]);
  const canGoForward = useMemo(() => hasValidStep(history, "forward", exists), [exists, history]);

  return useMemo(() => ({ canGoBack, canGoForward, history, navigateBackFromClosing, step }),
    [canGoBack, canGoForward, history, navigateBackFromClosing, step]);
}
