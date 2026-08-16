/**
 * Row windowing for the Explorer tree.
 *
 * Below [`WINDOW_ROW_THRESHOLD`] the tree mounts every row, which is exactly
 * what it always did: the ordinary workspace is a few dozen rows and windowing
 * a short list buys nothing while adding a scroll-position dependency to every
 * interaction. Above it, a 4,096-entry directory is roughly 33,000 DOM elements
 * — enough to put expand-to-paint and every subsequent focus move over budget —
 * so the tree mounts one contiguous band plus overscan and reserves the rest as
 * height.
 *
 * The band is deliberately contiguous and always contains the focused row, so
 * roving focus, Shift+F10, and scroll-to-item keep working against a row that
 * is really in the DOM.
 */
export const WINDOW_ROW_THRESHOLD = 200;

/** Used until a real row has been measured, and wherever layout has no height. */
export const DEFAULT_ROW_HEIGHT = 22;

/** Rows kept mounted beyond the viewport, so a wheel tick paints no blanks. */
export const OVERSCAN_ROWS = 12;

/** Assumed viewport when layout has not reported one yet. */
const DEFAULT_VIEWPORT_HEIGHT = 480;

export interface RowWindowInput {
  rowCount: number;
  rowHeight: number;
  scrollTop: number;
  viewportHeight: number;
  /** Kept mounted even when scrolled away from, so it stays focusable. */
  focusIndex: number;
}

export interface RowWindow {
  start: number;
  end: number;
  /** Height reserved above the mounted band. */
  leadingHeight: number;
  /** Height reserved below it. */
  trailingHeight: number;
  windowed: boolean;
}

export function rowWindow(input: RowWindowInput): RowWindow {
  const { rowCount } = input;
  if (rowCount <= WINDOW_ROW_THRESHOLD) {
    return { start: 0, end: rowCount, leadingHeight: 0, trailingHeight: 0, windowed: false };
  }
  const rowHeight = input.rowHeight > 0 ? input.rowHeight : DEFAULT_ROW_HEIGHT;
  const viewportHeight = input.viewportHeight > 0 ? input.viewportHeight : DEFAULT_VIEWPORT_HEIGHT;
  const visible = Math.ceil(viewportHeight / rowHeight);
  const span = Math.min(rowCount, visible + OVERSCAN_ROWS * 2);
  let start = Math.floor(Math.max(0, input.scrollTop) / rowHeight) - OVERSCAN_ROWS;
  let end = start + span;
  const focus = clamp(input.focusIndex, 0, rowCount - 1);
  if (focus < start) {
    start = focus - OVERSCAN_ROWS;
    end = start + span;
  } else if (focus >= end) {
    end = focus + OVERSCAN_ROWS + 1;
    start = end - span;
  }
  start = clamp(start, 0, Math.max(0, rowCount - span));
  end = clamp(start + span, 0, rowCount);
  return {
    start,
    end,
    leadingHeight: start * rowHeight,
    trailingHeight: (rowCount - end) * rowHeight,
    windowed: true,
  };
}

/**
 * The scroll offset that brings one row fully into view, or `undefined` when it
 * already is. Returning `undefined` matters: assigning `scrollTop` on a row the
 * user can already see would yank the viewport on every arrow key.
 */
export function scrollOffsetForRow(input: {
  index: number;
  rowHeight: number;
  scrollTop: number;
  viewportHeight: number;
}): number | undefined {
  const rowHeight = input.rowHeight > 0 ? input.rowHeight : DEFAULT_ROW_HEIGHT;
  const viewportHeight = input.viewportHeight > 0 ? input.viewportHeight : DEFAULT_VIEWPORT_HEIGHT;
  const top = input.index * rowHeight;
  const bottom = top + rowHeight;
  if (top < input.scrollTop) return top;
  if (bottom > input.scrollTop + viewportHeight) return bottom - viewportHeight;
  return undefined;
}

function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(high, value));
}
