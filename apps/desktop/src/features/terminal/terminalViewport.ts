import type { TerminalSize } from "./cellMetrics";

/** A viewport position that can be restored only against the grid that produced it. */
export interface TerminalViewportAnchor {
  atBottom: boolean;
  viewportLine: number;
  grid: TerminalSize;
}

type ViewportBuffer = {
  type: "normal" | "alternate";
  viewportY: number;
  baseY: number;
  cursorY: number;
};

/** The public xterm surface shared by the browser and headless renderers. */
type ViewportTerminal = {
  cols: number;
  rows: number;
  buffer: { active: ViewportBuffer };
  registerMarker(cursorYOffset: number): { line: number; dispose(): void } | undefined;
  resize(columns: number, rows: number): void;
  scrollToBottom(): void;
  scrollToLine(line: number): void;
};

type ViewportMarker = { line: number; dispose(): void };

export interface TerminalViewportBookmark {
  atBottom: boolean;
  marker?: ViewportMarker;
}

export function captureTerminalViewport(terminal: ViewportTerminal): TerminalViewportAnchor {
  const buffer = terminal.buffer.active;
  return {
    atBottom: buffer.viewportY >= buffer.baseY,
    viewportLine: buffer.viewportY,
    grid: { columns: terminal.cols, rows: terminal.rows },
  };
}

/** Captures a logical line before CSS is allowed to disturb xterm's viewport. */
export function bookmarkTerminalViewport(terminal: ViewportTerminal): TerminalViewportBookmark {
  const buffer = terminal.buffer.active;
  const atBottom = buffer.viewportY >= buffer.baseY;
  return {
    atBottom,
    marker: !atBottom && buffer.type === "normal"
      ? terminal.registerMarker(buffer.viewportY - (buffer.baseY + buffer.cursorY))
      : undefined,
  };
}

function restoreTerminalViewportBookmark(terminal: ViewportTerminal, bookmark: TerminalViewportBookmark): void {
  const buffer = terminal.buffer.active;
  const line = bookmark.marker?.line;
  if (bookmark.atBottom) terminal.scrollToBottom();
  else if (typeof line === "number" && Number.isInteger(line) && line >= 0 && line <= buffer.baseY) {
    terminal.scrollToLine(line);
  }
  else terminal.scrollToBottom();
}

export function disposeTerminalViewportBookmark(bookmark: TerminalViewportBookmark): void {
  bookmark.marker?.dispose();
}

/** Resizes while keeping the logical row at the top, even when wrapping moves it. */
export function resizeTerminalPreservingViewport(
  terminal: ViewportTerminal,
  size: TerminalSize,
  prepared = bookmarkTerminalViewport(terminal),
): void {
  try {
    terminal.resize(size.columns, size.rows);
    restoreTerminalViewportBookmark(terminal, prepared);
  } finally {
    disposeTerminalViewportBookmark(prepared);
  }
}

/** Restores a prepared marker without requiring a cell-grid change. */
export function restoreBookmarkedTerminalViewport(
  terminal: ViewportTerminal,
  prepared: TerminalViewportBookmark,
): void {
  try {
    restoreTerminalViewportBookmark(terminal, prepared);
  } finally {
    disposeTerminalViewportBookmark(prepared);
  }
}

/** Restores a serialized buffer's numeric line only when its grid still agrees. */
export function restoreTerminalViewport(terminal: ViewportTerminal, anchor: TerminalViewportAnchor): void {
  const buffer = terminal.buffer.active;
  const sameGrid = anchor.grid.columns === terminal.cols && anchor.grid.rows === terminal.rows;
  const validLine = Number.isInteger(anchor.viewportLine)
    && anchor.viewportLine >= 0
    && anchor.viewportLine <= buffer.baseY;
  if (anchor.atBottom) terminal.scrollToBottom();
  else if (buffer.type === "normal" && sameGrid && validLine) terminal.scrollToLine(anchor.viewportLine);
  else terminal.scrollToBottom();
}
