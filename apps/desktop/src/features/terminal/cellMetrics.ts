import type { Terminal } from "@xterm/xterm";

/**
 * Everything that turns pixels into a terminal grid, and nothing that draws one.
 *
 * This is the arithmetic the tmux client size is derived from — one cell's size
 * read out of xterm's own render service, and the cells that fit a box. It is
 * separated from the renderer because it is pure, because it is what
 * `measureBox.test.ts` and `clientSize.ts` actually exercise, and because a
 * mistake in it is measured in whole rows of the user's terminal rather than
 * in pixels.
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
    _coreBrowserService?: {
      onDprChange?: (listener: () => void) => { dispose(): void };
    };
    _renderService?: {
      dimensions?: { css?: { cell?: Partial<PixelBox> } };
      onDimensionsChange?: (listener: () => void) => { dispose(): void };
    };
    _charSizeService?: {
      height?: number;
      onCharSizeChange?: (listener: () => void) => { dispose(): void };
    };
  };
};

/**
 * Returns xterm's multiplier for the nearest practical row that is whole in
 * both CSS and device pixels. WebGL otherwise rounds the backing canvas and
 * its CSS height independently, stretching an odd-sized grid by one pixel at
 * fractional DPR. The small bound keeps an unusual display ratio from buying
 * correctness with conspicuously loose lines.
 */
export function deviceSafeLineHeight(
  measuredCharHeight: number | undefined,
  devicePixelRatio: number,
): number | undefined {
  if (!(measuredCharHeight !== undefined && measuredCharHeight > 0)) return undefined;
  const ratio = devicePixelRatio > 0 ? devicePixelRatio : 1;
  const deviceCharHeight = Math.ceil(measuredCharHeight * ratio);
  const nativeCssHeight = deviceCharHeight / ratio;
  const firstCssRow = Math.ceil(nativeCssHeight);
  const lastCssRow = firstCssRow + 3;
  for (let cssRow = firstCssRow; cssRow <= lastCssRow; cssRow += 1) {
    const deviceRow = cssRow * ratio;
    if (!Number.isInteger(deviceRow)) continue;
    if (deviceRow === deviceCharHeight) return 1;
    // xterm floors this product. Aim inside the target integer, not on the
    // floating-point boundary below it.
    return (deviceRow + 0.5) / deviceCharHeight;
  }
  return 1;
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
