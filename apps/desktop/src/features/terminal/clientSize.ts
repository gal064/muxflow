import type { PixelBox, TerminalSize } from "./TerminalRenderer";

/**
 * Smallest and largest tmux client the desktop is ever allowed to ask for.
 *
 * The upper bound is a blast radius, not a capability claim. tmux obeys
 * `refresh-client -C` for every client that participates in sizing, so the
 * request reaches the user's *real* windows — including the ones their plain
 * terminals are attached to. A request of 108x298 is what P12-U006 did to four
 * of them. No display asks for 500 cells on an axis, so a computation that ever
 * produces one is wrong, and a wrong computation must fail loudly and locally
 * instead of resizing somebody's work.
 */
export const MIN_CLIENT_CELLS = 2;
export const MAX_CLIENT_CELLS = 500;

/**
 * Border plus padding of one `.pane-frame`, per axis, in CSS pixels
 * (`styles.css`: `.pane-frame { … padding: 1px; border: 1px solid … }` — both
 * sides of each axis, so 2 × (1 + 1)). It is a constant rather than a
 * measurement because the frame it describes belongs to a pane, and no pane may
 * enter this computation; if the rule changes, the request is off by half a
 * column, which is the smallest failure mode in this file.
 *
 * Subtracting exactly one frame's chrome is deliberate. tmux spends one cell per
 * split on a divider, and the app spends one frame's chrome per extra pane, so
 * the two roughly cancel and the estimate stays honest as panes are added. It
 * errs low — asking for slightly less than the surface could show — which is the
 * safe direction: a pane rendered at tmux's grid inside a box that is a cell
 * too large shows a margin, while one cell too small hides a column.
 */
export const PANE_FRAME_CHROME_PIXELS = 4;

/**
 * What the desktop decided to ask tmux for, or why it decided to ask nothing.
 *
 * `refused` is a bug report, not a fallback: it is never clamped into range and
 * sent anyway, because a size nobody computed on purpose is exactly what damaged
 * the user's windows.
 */
export type ClientSizeDecision =
  | { kind: "size"; size: TerminalSize }
  | { kind: "unmeasurable"; reason: string }
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
 * is the whole computation. `measureBox` performs the division with the
 * terminal's own cell metrics, padding and scrollbar allowance — the same
 * quantities `FitAddon.proposeDimensions` uses for a pane.
 */
export function clientSizeForSurface(
  surface: PixelBox | undefined,
  measureBox: (box: PixelBox) => TerminalSize | undefined,
): ClientSizeDecision {
  if (!surface || !Number.isFinite(surface.width) || !Number.isFinite(surface.height)) {
    return { kind: "unmeasurable", reason: "the terminal surface has no measured pixel box yet" };
  }
  const box = {
    width: surface.width - PANE_FRAME_CHROME_PIXELS,
    height: surface.height - PANE_FRAME_CHROME_PIXELS,
  };
  if (box.width <= 0 || box.height <= 0) {
    return { kind: "unmeasurable", reason: "the terminal surface is not visible" };
  }
  const measured = measureBox(box);
  if (!measured) {
    return { kind: "unmeasurable", reason: "no terminal has usable cell metrics yet" };
  }
  const { columns, rows } = measured;
  if (!Number.isInteger(columns) || !Number.isInteger(rows)) {
    return { kind: "refused", reason: `Refusing a ${columns}x${rows} tmux client size: it is not a whole number of cells.` };
  }
  if (!inRange(columns) || !inRange(rows)) {
    return {
      kind: "refused",
      reason: `Refusing to resize the tmux client to ${columns}x${rows}: outside the ${MIN_CLIENT_CELLS}–${MAX_CLIENT_CELLS} cell bound for a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface.`,
    };
  }
  return { kind: "size", size: { columns, rows } };
}

function inRange(cells: number): boolean {
  return cells >= MIN_CLIENT_CELLS && cells <= MAX_CLIENT_CELLS;
}
