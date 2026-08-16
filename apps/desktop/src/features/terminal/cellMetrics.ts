import type { IDisposable, Terminal } from "@xterm/xterm";

/**
 * Everything that turns pixels into a terminal grid, and nothing that draws one.
 *
 * This is the arithmetic the tmux client size is derived from — the row pitch
 * the token asks for expressed in xterm's units, one cell's size read out of
 * xterm's own render service, and the cells that fit a box. It is separated from
 * the renderer because it is pure, because it is what `measureBox.test.ts` and
 * `clientSize.ts` actually exercise, and because a mistake in it is measured in
 * whole rows of the user's terminal rather than in pixels.
 */

export interface TerminalSize {
  columns: number;
  rows: number;
}

/** A CSS-pixel box. The outer box of an element, borders and padding included. */
export interface PixelBox {
  width: number;
  height: number;
}


/** Chrome a terminal spends out of the box it is given, in CSS pixels. */
export interface TerminalBoxChrome {
  horizontal: number;
  vertical: number;
  scrollbar: number;
}

/**
 * A terminal, plus the internal xterm does not expose: its render service's CSS
 * cell size. The dependency is in the signature rather than inside a cast so
 * that "this reads xterm internals" is visible to the next reader and to the
 * next upgrade.
 */
export type MeasurableTerminal = Pick<Terminal, "options"> & {
  _core?: {
    _renderService?: { dimensions?: { css?: { cell?: Partial<PixelBox> } } };
    /** What xterm measured one character to be, in CSS pixels. See `xtermLineHeight`. */
    _charSizeService?: {
      height?: number;
      onCharSizeChange?: (listener: () => void) => IDisposable;
    };
  };
};

/**
 * The `lineHeight` option that makes xterm render rows at `rowPitch` CSS pixels.
 *
 * xterm's `lineHeight` is **not** CSS's. It multiplies the *measured character
 * height* — `device.cell.height = floor(device.char.height * lineHeight)` in
 * `RenderService._updateDimensions` — and a monospace face measures taller than
 * its font size (JetBrains Mono at 13 px measures ~17 px). Handing xterm the
 * token's 1.42 therefore asked for 17 × 1.42 ≈ 24 px rows: a real pitch of
 * ~1.86 font sizes, a terminal that looks stretched, and a tmux grid a third
 * shorter than the window can hold — 30 rows where 41 belong.
 *
 * So the token stays the design value (row pitch = font size × 1.42) and this
 * expresses it in xterm's units. Only the cell metric changes; every derivation
 * downstream — `FitAddon`, `terminalMeasurements`, `clientSizeForSurface` —
 * reads the resulting `css.cell`, so all of them stay in agreement by
 * construction.
 *
 * A row is a whole number of *device* pixels, and xterm floors into them, so the
 * multiplier aims at the middle of the device row it wants rather than at its
 * edge, via `wholeDeviceRowHeight`.
 *
 * Returns undefined when there is nothing to derive from, which leaves xterm at
 * its unit multiplier: rows one measured character tall, slightly tighter than
 * the design, never a grid the surface cannot show.
 */
export function xtermLineHeight(
  rowPitch: number,
  measuredCharHeight: number | undefined,
  devicePixelRatio = 1,
): number | undefined {
  if (!(rowPitch > 0) || !(measuredCharHeight !== undefined && measuredCharHeight > 0)) return undefined;
  const ratio = devicePixelRatio > 0 ? devicePixelRatio : 1;
  // The same rounding xterm performs on the measured character.
  const deviceCharHeight = Math.ceil(measuredCharHeight * ratio);
  // Never below the face: xterm refuses a multiplier under 1, and the row that
  // clamp produces is `deviceCharHeight` — a number nothing chose, and as likely
  // to be the odd one this whole function exists to avoid.
  return (unresampledRowHeight(rowPitch, ratio, deviceCharHeight) + 0.5) / deviceCharHeight;
}

/**
 * How much of the designed pitch a row may give up to avoid being resampled.
 *
 * At a ratio of 1.25 the smallest row that is whole in both spaces is a multiple
 * of 4 CSS px, which would drag a 18.46 px pitch to 20 — 8% of the design, and a
 * worse trade than the resampling. So the snap is only taken when it is cheap,
 * and the budget is stated here rather than falling out of an integer check.
 */
const MAX_PITCH_SACRIFICE_PX = 1;

/**
 * The device row height to aim at: as close to the token's pitch as a row can be
 * while `rows × row` still divides by the device pixel ratio exactly.
 *
 * That constraint is not cosmetic. xterm sizes the WebGL canvas's backing store
 * from `rows × device.cell.height` but its CSS box from
 * `Math.round(that / devicePixelRatio)`, and a `DevicePixelObserver` then resizes
 * the backing store to whatever that rounded box actually measures. So whenever
 * the product does not divide exactly, the two disagree by up to one device
 * pixel, the glyph quads are placed in a clip space one pixel shorter than the
 * viewport they are drawn into, and the whole grid is stretched by that pixel:
 * every row lands on a different subpixel offset, the GPU's linear filter smears
 * each one differently, and text that is crisp at the top of the pane is visibly
 * soft and displaced by the bottom.
 *
 * Measured on the packaged app at 13 px / 1.42 on a 2× display: a 37-device-pixel
 * row (18.5 CSS px) drifted each row's glyph centroid by 0.0257 device px, 1.0 px
 * across the 39-row pane, against a within-row spread of 0.003 px.
 *
 * A row survives every grid size when it is whole in both spaces and tall enough
 * for the face, so the rule is those three predicates over the handful of whole
 * CSS rows the budget admits, nearest the pitch first. 13 × 1.42 = 18.46 CSS px,
 * so a 2× display gets 18.0 — 1.385 font sizes rather than the token's 1.42 —
 * and so does a 1.5× display, where it is 27 device pixels and exact.
 *
 * When no candidate qualifies, the nearest device row stands and the grid is
 * stretched: at 1.25 the nearest whole row in both spaces is a multiple of 4 CSS
 * px, which would drag 18.46 to 20, and that is the worse trade. Stated by the
 * budget, rather than falling out of a rounding.
 */
function unresampledRowHeight(rowPitch: number, ratio: number, deviceCharHeight: number): number {
  for (const cssRow of rowsWithinBudget(rowPitch)) {
    if (Number.isInteger(cssRow * ratio) && cssRow * ratio >= deviceCharHeight) return cssRow * ratio;
  }
  // Never below the face: xterm refuses a multiplier under 1, and the row that
  // clamp produces is `deviceCharHeight` — a number nothing chose.
  return Math.max(deviceCharHeight, Math.round(rowPitch * ratio));
}

/** Whole CSS rows within the budget, nearest the pitch first; a tie goes taller. */
function rowsWithinBudget(rowPitch: number): number[] {
  const rows: number[] = [];
  for (let row = Math.max(1, Math.ceil(rowPitch - MAX_PITCH_SACRIFICE_PX)); row <= Math.floor(rowPitch + MAX_PITCH_SACRIFICE_PX); row += 1) {
    rows.push(row);
  }
  return rows.sort((left, right) => Math.abs(left - rowPitch) - Math.abs(right - rowPitch) || right - left);
}

/** Everything needed to turn a pixel box into a terminal grid. */
export interface TerminalMeasurements {
  cell: PixelBox;
  chrome: TerminalBoxChrome;
}

/**
 * Reads a terminal's cell size and chrome from the DOM and from xterm's own
 * render service — the same places `FitAddon.proposeDimensions` reads them.
 *
 * Exported and parameterised so the reading, not a reimplementation of it, is
 * what the tests exercise: `measureBox.test.ts` runs this against a real
 * `Terminal` and asserts it agrees with `FitAddon`. Everything is
 * optional-chained: if a future xterm moves the render service, this reports
 * nothing and the app asks tmux for nothing, which is the safe outcome.
 */
export function terminalMeasurements(
  terminal: MeasurableTerminal,
  host: Element,
  element: Element,
): TerminalMeasurements | undefined {
  const cell = terminal._core?._renderService?.dimensions?.css?.cell;
  if (!cell?.width || !cell.height) return undefined;
  const hostStyle = window.getComputedStyle(host);
  const terminalStyle = window.getComputedStyle(element);
  return {
    cell: { width: cell.width, height: cell.height },
    chrome: {
      horizontal: edges(hostStyle, "left", "right") + edges(terminalStyle, "left", "right"),
      vertical: edges(hostStyle, "top", "bottom") + edges(terminalStyle, "top", "bottom"),
      // xterm reserves this on the right whenever there is scrollback, and
      // FitAddon subtracts it before dividing; a terminal sized without it
      // renders its last columns under the scrollbar.
      scrollbar: terminal.options.scrollback === 0 ? 0 : terminal.options.overviewRuler?.width || 14,
    },
  };
}

/**
 * Cells that fit a pixel box, given one cell's size and the terminal's own
 * chrome. Extracted from `measureBox` so the arithmetic that decides how big a
 * tmux client to ask for is testable without a DOM: it is the arithmetic
 * `FitAddon.proposeDimensions` performs, with an explicit box.
 *
 * Floors, never rounds. Half a cell of terminal is not a cell of terminal, and
 * rounding up asks tmux for a grid the surface cannot show — which is how a
 * pane ends up with its bottom row cut off.
 */
export function cellsForBox(
  box: PixelBox,
  cell: PixelBox,
  chrome: TerminalBoxChrome,
): TerminalSize | undefined {
  if (!(cell.width > 0) || !(cell.height > 0)) return undefined;
  const width = box.width - chrome.horizontal - chrome.scrollbar;
  const height = box.height - chrome.vertical;
  if (!(width > 0) || !(height > 0)) return undefined;
  return { columns: Math.floor(width / cell.width), rows: Math.floor(height / cell.height) };
}

/**
 * Padding plus border an element spends on the named sides, in CSS pixels.
 *
 * A border with no style spends nothing. Browsers already compute its width to
 * `0px`, so this guard changes nothing in the app; jsdom reports the initial
 * `medium` (16 px) instead, and without it `measureBox.test.ts` would be
 * asserting against 64 px of border that does not exist anywhere.
 */
function edges(style: CSSStyleDeclaration, ...sides: Array<"top" | "bottom" | "left" | "right">): number {
  return sides.reduce((total, side) => {
    const padding = pixels(style.getPropertyValue(`padding-${side}`));
    const invisible = ["none", "hidden", ""].includes(style.getPropertyValue(`border-${side}-style`));
    return total + padding + (invisible ? 0 : pixels(style.getPropertyValue(`border-${side}-width`)));
  }, 0);
}

function pixels(value: string): number {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}
