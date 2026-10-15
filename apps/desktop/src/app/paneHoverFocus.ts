import { useCallback, useEffect, useRef, type PointerEvent } from "react";
import { pointerDragActive, pointerDragEndedAt } from "../features/dragDrop/pointerDrag";

/**
 * How long the pointer must rest on a pane before it takes focus. Long enough
 * that sweeping across a split on the way somewhere else focuses nothing.
 */
export const HOVER_FOCUS_DELAY_MS = 120;
/**
 * Releasing a drag's pointer capture enters whatever pane is under it. That
 * enter is the drop, not a hover: focusing that pane would take focus from
 * the pane the drop just joined beside it.
 */
const AFTER_DRAG_QUIET_MS = 300;

/** The id hover gives the document docked beside the panes, which it moves to and from like a pane. */
export const DOCK_HOVER_TARGET = "dock";

export interface PaneHoverFocusOptions {
  /** The setting, already folded with "no dialog or menu is open" and "the app window has focus". */
  enabled: boolean;
  activePaneId?: string;
  /** Whether the keyboard is in the docked document; the active pane does not hold it then. */
  dockHoldsFocus?(): boolean;
  focus(paneId: string): void;
}

export interface PaneHoverHandlers {
  onPointerEnter(event: PointerEvent<HTMLElement>): void;
  onPointerLeave(): void;
  onPointerDown(): void;
}

/**
 * Focus-follows-mouse between split panes.
 *
 * Every check is repeated when the timer fires rather than trusted from the
 * enter, because the delay is exactly when a dialog can open or another drag
 * can start. Focus is never taken from a field: a pointer drifting over a
 * split must not pull the caret out of the search box, a dialog's input, or
 * an editor outside the dock. A button keeps nothing worth protecting — closing Settings
 * leaves focus on its gear, and that must not switch the feature off.
 */
export function usePaneHoverFocus(options: PaneHoverFocusOptions): (paneId: string) => PaneHoverHandlers {
  const latest = useRef(options);
  latest.current = options;
  const timer = useRef<number | undefined>(undefined);
  const cancel = useCallback(() => {
    if (timer.current !== undefined) window.clearTimeout(timer.current);
    timer.current = undefined;
  }, []);
  useEffect(() => cancel, [cancel]);
  useEffect(() => {
    if (!options.enabled) cancel();
  }, [cancel, options.enabled]);

  return useCallback((paneId: string): PaneHoverHandlers => ({
    onPointerEnter(event) {
      cancel();
      // A held button is a selection or a divider drag crossing panes.
      if (event.buttons !== 0 || !latest.current.enabled) return;
      if (performance.now() - pointerDragEndedAt() < AFTER_DRAG_QUIET_MS) return;
      timer.current = window.setTimeout(() => {
        timer.current = undefined;
        const current = latest.current;
        const inDock = current.dockHoldsFocus?.() ?? false;
        const holds = paneId === DOCK_HOVER_TARGET ? inDock : !inDock && current.activePaneId === paneId;
        if (!current.enabled || holds || pointerDragActive()) return;
        if (focusIsInAField()) return;
        current.focus(paneId);
      }, HOVER_FOCUS_DELAY_MS);
    },
    onPointerLeave: cancel,
    onPointerDown: cancel,
  }), [cancel]);
}

/**
 * A terminal's own input textarea is not a field: it is the thing hover moves
 * between. Nor is the docked document's editor, which sits beside the panes as
 * one more of them.
 */
function focusIsInAField(): boolean {
  const focused = document.activeElement;
  if (!(focused instanceof HTMLElement) || focused.closest("[data-terminal-surface]")) return false;
  // Only the document input yields to hover. Find/replace and other fields
  // inside the dock still keep the keyboard while the user types in them.
  if (focused.closest("[data-docked-tab]")
    && focused.matches(".cm-content, .monaco-editor textarea.inputarea, .monaco-editor .native-edit-context")) return false;
  return focused.isContentEditable || focused.matches("input, textarea, select");
}
