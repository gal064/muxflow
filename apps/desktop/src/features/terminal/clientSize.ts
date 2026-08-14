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
 * instead of resizing somebody's work. The host enforces the same range
 * (`TERMINAL_CLIENT_CELL_BOUNDS` in `service/terminal.rs`); the lane's
 * `boundProbe` exercises that side deliberately, so the two cannot drift
 * unnoticed.
 */
export const MIN_CLIENT_CELLS = 2;
export const MAX_CLIENT_CELLS = 500;

/**
 * Chrome between the tiled surface's box and a pane's terminal, per axis, in
 * CSS pixels: the two 1px borders of one `.pane-frame`.
 *
 * Everything else in the frame is deliberately zero. Per-pane chrome does not
 * shrink with the pane, so a pane holding a fraction `f` of the window gets
 * only `f` of the surface's allowance back while spending the whole of its own
 * — the deficit is `chrome × (1 − f)`, and with 14 px of chrome (the frame plus
 * a 6 px terminal padding) a half-height pane came up ~7 px short and clipped
 * its bottom row. The surface's own breathing room is the `inset` on
 * `.terminal-window`, which is outside the box measured here and therefore
 * costs the panes nothing.
 */
export const PANE_FRAME_CHROME_PIXELS = 2;

/**
 * What the desktop decided to ask tmux for, or why it decided to ask nothing.
 *
 * `refused` is a bug report: a size above the bound is a defect in this
 * computation, it is never clamped into range and sent anyway, and the user is
 * told. `unavailable` is ordinary — no metrics yet, no visible surface, a
 * window dragged too small to hold a terminal — and stays quiet.
 */
export type ClientSizeDecision =
  | { kind: "size"; size: TerminalSize }
  | { kind: "unavailable"; reason: string; retry: boolean }
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
    return { kind: "unavailable", reason: "the terminal surface has no measured pixel box yet", retry: false };
  }
  const box = {
    width: surface.width - PANE_FRAME_CHROME_PIXELS,
    height: surface.height - PANE_FRAME_CHROME_PIXELS,
  };
  if (box.width <= 0 || box.height <= 0) {
    return { kind: "unavailable", reason: "the terminal surface is not visible", retry: false };
  }
  const measured = measureBox(box);
  if (!measured) {
    // Either no terminal has reported cell metrics yet — which a retry fixes
    // once one mounts — or the surface is smaller than a single cell, which it
    // does not.
    return {
      kind: "unavailable",
      reason: `no terminal could measure a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface in cells`,
      retry: true,
    };
  }
  const { columns, rows } = measured;
  if (!Number.isInteger(columns) || !Number.isInteger(rows)) {
    return { kind: "refused", reason: `Refusing a ${columns}x${rows} tmux client size: it is not a whole number of cells.` };
  }
  if (columns > MAX_CLIENT_CELLS || rows > MAX_CLIENT_CELLS) {
    return {
      kind: "refused",
      reason: `Refusing to resize the tmux client to ${columns}x${rows}: above the ${MAX_CLIENT_CELLS} cell bound for a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface.`,
    };
  }
  if (columns < MIN_CLIENT_CELLS || rows < MIN_CLIENT_CELLS) {
    // An ordinary consequence of dragging the window small. Nothing is sent,
    // and nothing is said: the user can see how big their own window is.
    return { kind: "unavailable", reason: `${columns}x${rows} is too small to be a terminal`, retry: false };
  }
  return { kind: "size", size: { columns, rows } };
}
