import type { Pane } from "../../app/types";
import { cellsForBox, type PixelBox, type TerminalMeasurements, type TerminalSize } from "./TerminalRenderer";

/**
 * Smallest and largest tmux client the desktop is ever allowed to ask for.
 *
 * The upper bound is a blast radius, not a capability claim. tmux obeys
 * `refresh-client -C` for every client that participates in sizing, so the
 * request reaches the user's *real* windows — including the ones their plain
 * terminals are attached to. A request of 108x298 is what P12-U006 did to four
 * of them. No display asks for 500 cells on an axis, so a computation that ever
 * produces one is wrong, and a wrong computation must fail loudly and locally
 * instead of resizing somebody's work. The host enforces the same range
 * (`TERMINAL_CLIENT_CELL_BOUNDS` in `service/terminal.rs`); the lane's
 * `boundProbe` exercises that side deliberately, so the two cannot drift
 * unnoticed.
 */
export const MIN_CLIENT_CELLS = 2;
export const MAX_CLIENT_CELLS = 500;

/**
 * What the desktop decided to ask tmux for, or why it decided to ask nothing.
 *
 * `refused` is a bug report: a size above the bound is a defect in this
 * computation, it is never clamped into range and sent anyway, and the user is
 * told. `none` is ordinary — no metrics yet, no visible surface, a window
 * dragged too small to hold a terminal — and carries no message because nothing
 * reads one.
 */
export type ClientSizeDecision =
  | { kind: "size"; size: TerminalSize }
  | { kind: "none" }
  | { kind: "refused"; reason: string };

/**
 * The tmux client size for a workspace, derived from the tiled terminal
 * surface's own pixel box and the renderer's cell metrics.
 *
 * Pane geometry, pane boxes and zoom state are deliberately not parameters
 * (P12-U006). The previous computation scaled the active pane's measured box by
 * its share of the topology — `rows = measured.rows * grid.height / pane.height`
 * — which holds only while the CSS box occupies exactly that share. Under pane
 * zoom the box is the whole window while the pane's topology height is one
 * split, and during a layout change the two disagree transiently; both cases
 * multiply the request instead of describing it, and tmux obeys.
 *
 * The surface is one tmux window's worth of pixels, so dividing it by one cell
 * is the whole computation. The terminal's own measurements — cell size,
 * padding, scrollbar allowance — arrive as values from whichever terminal is
 * alive; they describe a terminal, not a pane.
 */
export function clientSizeForSurface(
  surface: PixelBox | undefined,
  measurements: TerminalMeasurements | undefined,
): ClientSizeDecision {
  if (!surface || !Number.isFinite(surface.width) || !Number.isFinite(surface.height) || !measurements) {
    return { kind: "none" };
  }
  // The surface's box is spent in full. A pane frame draws its edge with an
  // inset shadow and its terminal has no padding, so nothing between this box
  // and a pane's terminal consumes a pixel — which is what lets a pane always
  // render the grid tmux derived from this number, at any split (`styles.css`).
  const measured = cellsForBox(surface, measurements.cell, measurements.chrome);
  // Smaller than one cell of terminal, which a window can legitimately be.
  if (!measured) return { kind: "none" };
  const { columns, rows } = measured;
  if (columns > MAX_CLIENT_CELLS || rows > MAX_CLIENT_CELLS) {
    return {
      kind: "refused",
      reason: `Refusing to resize the tmux client to ${columns}x${rows}: above the ${MAX_CLIENT_CELLS} cell bound for a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface.`,
    };
  }
  // Dragging the window small is not a defect. Nothing is sent, and nothing is
  // said: the user can see how big their own window is.
  if (columns < MIN_CLIENT_CELLS || rows < MIN_CLIENT_CELLS) return { kind: "none" };
  return { kind: "size", size: { columns, rows } };
}

/**
 * The cell grid tmux *actually* gave a window, read off the authoritative
 * snapshot.
 *
 * The counterpart to `clientSizeForSurface`, and deliberately a different
 * quantity: that one is what the desktop believes the window should be, this
 * one is what tmux settled on. They come apart whenever another client wins —
 * under tmux's `window-size latest`, any activity in a plain terminal attached
 * to the same session resizes the shared windows to *its* size, and the desktop
 * has nothing to notice that with, because its own surface never moved. So it
 * letterboxes, and stays letterboxed.
 *
 * Derived from panes rather than a window field because the wire carries pane
 * geometry and not window geometry. The bottom-right pane's offset plus its own
 * size is the window's size exactly: tmux counts the separator rows and columns
 * into the following pane's offset, so the sum lands on the edge whatever the
 * split. Zoom is not a special case here — a zoomed pane covers the window, so
 * its own box is the window's box.
 */
export function windowCellSize(panes: readonly Pane[], windowId: string | undefined): TerminalSize | undefined {
  if (!windowId) return undefined;
  let columns = 0;
  let rows = 0;
  for (const pane of panes) {
    if (pane.windowId !== windowId) continue;
    columns = Math.max(columns, pane.left + pane.width);
    rows = Math.max(rows, pane.top + pane.height);
  }
  // A window with no panes in the snapshot is a window this cannot describe,
  // which must read as "no answer" rather than as a zero-sized window: the
  // caller compares this against a requested size and would see every request
  // as unhonoured.
  return columns > 0 && rows > 0 ? { columns, rows } : undefined;
}
