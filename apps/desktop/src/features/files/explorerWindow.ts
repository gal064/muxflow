/**
 * Row windowing for the Explorer tree.
 *
 * Below [`WINDOW_ROW_THRESHOLD`] the tree mounts every row, which is exactly
 * what it always did: the ordinary workspace is a few dozen rows and windowing
 * a short list buys nothing while adding a scroll-position dependency to every
 * interaction. Above it, a 4,096-entry directory is roughly 33,000 DOM elements
 * — enough to put expand-to-paint and every subsequent focus move over budget —
 * so the tree mounts what the viewport can see plus overscan, and reserves the
 * rest as height.
 *
 * The focused row is mounted *in addition to* that band rather than instead of
 * it. Moving the band to the focused row looks equivalent and is not: focus
 * does not follow the scrollbar, so scrolling a large directory with a wheel
 * would leave the mounted rows pinned at the top and the viewport showing
 * nothing at all, with no row left to click.
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

/** One contiguous run of mounted rows, and the height reserved before it. */
export interface RowSegment {
  start: number;
  end: number;
  leadingHeight: number;
}

export interface RowWindow {
  /** In index order, never overlapping. */
  segments: readonly RowSegment[];
  /** Height reserved after the last segment. */
  trailingHeight: number;
  windowed: boolean;
}

export function rowWindow(input: RowWindowInput): RowWindow {
  const { rowCount } = input;
  if (rowCount <= WINDOW_ROW_THRESHOLD) {
    return {
      segments: rowCount > 0 ? [{ start: 0, end: rowCount, leadingHeight: 0 }] : [],
      trailingHeight: 0,
      windowed: false,
    };
  }
  const rowHeight = input.rowHeight > 0 ? input.rowHeight : DEFAULT_ROW_HEIGHT;
  const viewportHeight = input.viewportHeight > 0 ? input.viewportHeight : DEFAULT_VIEWPORT_HEIGHT;
  const visible = Math.ceil(viewportHeight / rowHeight);
  const span = Math.min(rowCount, visible + OVERSCAN_ROWS * 2);
  const start = clamp(
    Math.floor(Math.max(0, input.scrollTop) / rowHeight) - OVERSCAN_ROWS,
    0,
    Math.max(0, rowCount - span),
  );
  const band: RowSegment = { start, end: clamp(start + span, 0, rowCount), leadingHeight: 0 };

  const focus = clamp(input.focusIndex, 0, rowCount - 1);
  const runs = focus >= band.start && focus < band.end
    ? [band]
    : focus < band.start
      ? [{ start: focus, end: focus + 1, leadingHeight: 0 }, band]
      : [band, { start: focus, end: focus + 1, leadingHeight: 0 }];

  let covered = 0;
  const segments = runs.map((run) => {
    const segment = { ...run, leadingHeight: (run.start - covered) * rowHeight };
    covered = run.end;
    return segment;
  });
  return {
    segments,
    trailingHeight: (rowCount - covered) * rowHeight,
    windowed: true,
  };
}

/** Total rows a window mounts, which is the cost the row budget is about. */
export function mountedRowCount(window: RowWindow): number {
  return window.segments.reduce((total, segment) => total + (segment.end - segment.start), 0);
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
