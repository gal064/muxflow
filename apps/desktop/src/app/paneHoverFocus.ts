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

export interface PaneHoverFocusOptions {
  /** The setting, already folded with "no dialog or menu is open" and "the app window has focus". */
  enabled: boolean;
  activePaneId?: string;
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
 * an editor. A button keeps nothing worth protecting — closing Settings
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
        if (!current.enabled || current.activePaneId === paneId || pointerDragActive()) return;
        if (focusIsInAField()) return;
        current.focus(paneId);
      }, HOVER_FOCUS_DELAY_MS);
    },
    onPointerLeave: cancel,
    onPointerDown: cancel,
  }), [cancel]);
}

/** A terminal's own input textarea is not a field: it is the thing hover moves between. */
function focusIsInAField(): boolean {
  const focused = document.activeElement;
  if (!(focused instanceof HTMLElement) || focused.closest("[data-terminal-surface]")) return false;
  return focused.isContentEditable || focused.matches("input, textarea, select");
}
