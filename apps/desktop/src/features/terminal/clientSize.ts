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
 * Chrome between the tiled surface's box and a pane's terminal, per axis, in
 * CSS pixels: the two 1px borders of one `.pane-frame` (`styles.css`).
 *
 * Everything else in the frame is deliberately zero, because per-pane chrome
 * does not shrink with the pane. A pane holding a fraction `f` of the window
 * gets only `f` of the surface's allowance back while spending the whole of its
 * own, so the shortfall is `chrome × (1 − f)`: with the 6 px terminal padding
 * this used to carry, a half-height pane came up ~7 px short and clipped its
 * bottom row. At 2 px the worst case is 2 px of a ~17 px cell. The surface's
 * breathing room now comes from the `inset` on `.terminal-window`, which is
 * outside the box measured here and costs the panes nothing.
 *
 * The horizontal axis carries one more term the surface pays once and each pane
 * spends: xterm's scrollbar allowance (`chrome.scrollbar`, 14 px). It does not
 * clip, because the allowance is subtracted from the whole surface while each
 * pane keeps its full box: for N panes across, a pane's element exceeds its
 * canvas by `(16 + r·cell − 2N)/N` px, which stays positive well past any
 * usable split. What it costs is margin — with many panes across, the last
 * column of each sits where a scrollbar would be drawn, and xterm's scrollbar
 * overlays rather than reserves.
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
  | { kind: "unavailable"; reason: string }
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
  if (!surface || !Number.isFinite(surface.width) || !Number.isFinite(surface.height)) {
    return { kind: "unavailable", reason: "the terminal surface has no measured pixel box yet" };
  }
  if (!measurements) {
    return { kind: "unavailable", reason: "no terminal has reported cell metrics yet" };
  }
  const measured = cellsForBox(
    {
      width: surface.width - PANE_FRAME_CHROME_PIXELS,
      height: surface.height - PANE_FRAME_CHROME_PIXELS,
    },
    measurements.cell,
    measurements.chrome,
  );
  if (!measured) {
    return {
      kind: "unavailable",
      reason: `a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface is smaller than one cell of terminal`,
    };
  }
  const { columns, rows } = measured;
  if (columns > MAX_CLIENT_CELLS || rows > MAX_CLIENT_CELLS) {
    return {
      kind: "refused",
      reason: `Refusing to resize the tmux client to ${columns}x${rows}: above the ${MAX_CLIENT_CELLS} cell bound for a ${Math.round(surface.width)}x${Math.round(surface.height)} pixel surface.`,
    };
  }
  if (columns < MIN_CLIENT_CELLS || rows < MIN_CLIENT_CELLS) {
    // An ordinary consequence of dragging the window small. Nothing is sent,
    // and nothing is said: the user can see how big their own window is.
    return { kind: "unavailable", reason: `${columns}x${rows} is too small to be a terminal` };
  }
  return { kind: "size", size: { columns, rows } };
}
