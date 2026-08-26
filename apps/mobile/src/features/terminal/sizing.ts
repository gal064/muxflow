// The sizing rule (design doc §9.5, D6): the terminal fills the WebView with
// whole cells. Shared by the WebView page (which measures) and the app (which
// sends RESIZE_TERMINAL and reasons about the numbers).

/** JetBrains Mono at 13 px, line-height 1.2: the nominal cell from §9.5. */
export const TERMINAL_FONT_SIZE_PX = 13;
export const TERMINAL_LINE_HEIGHT = 1.2;
export const NOMINAL_CELL = { width: 7.8, height: 15.6 } as const;

/** tmux refuses anything smaller; also keeps a keyboard-squashed viewport sane. */
export const MIN_COLS = 2;
export const MIN_ROWS = 1;

export interface CellSize { width: number; height: number }
export interface Grid { cols: number; rows: number }

/**
 * `cols = floor(width / cellWidth)`, `rows = floor(height / cellHeight)`,
 * clamped to the minimum tmux accepts. A zero or non-finite viewport (the
 * WebView before layout) yields the minimum grid rather than NaN.
 */
export function computeGrid(viewport: { width: number; height: number }, cell: CellSize = NOMINAL_CELL): Grid {
  const cols = Number.isFinite(viewport.width) && cell.width > 0 ? Math.floor(viewport.width / cell.width) : 0;
  const rows = Number.isFinite(viewport.height) && cell.height > 0 ? Math.floor(viewport.height / cell.height) : 0;
  return { cols: Math.max(MIN_COLS, cols), rows: Math.max(MIN_ROWS, rows) };
}

export function sameGrid(a: Grid | undefined, b: Grid | undefined): boolean {
  return a !== undefined && b !== undefined && a.cols === b.cols && a.rows === b.rows;
}
