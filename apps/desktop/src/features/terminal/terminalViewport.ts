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

export function captureTerminalViewport(terminal: ViewportTerminal): TerminalViewportAnchor {
  const buffer = terminal.buffer.active;
  return {
    atBottom: buffer.viewportY >= buffer.baseY,
    viewportLine: buffer.viewportY,
    grid: { columns: terminal.cols, rows: terminal.rows },
  };
}

/** Resizes while keeping the logical row at the top, even when wrapping moves it. */
export function resizeTerminalPreservingViewport(terminal: ViewportTerminal, size: TerminalSize): void {
  const buffer = terminal.buffer.active;
  const wasAtBottom = buffer.viewportY >= buffer.baseY;
  const marker = !wasAtBottom && buffer.type === "normal"
    ? terminal.registerMarker(buffer.viewportY - (buffer.baseY + buffer.cursorY))
    : undefined;
  try {
    terminal.resize(size.columns, size.rows);
    const resized = terminal.buffer.active;
    if (wasAtBottom) terminal.scrollToBottom();
    else if (marker && Number.isInteger(marker.line) && marker.line >= 0 && marker.line <= resized.baseY) {
      terminal.scrollToLine(marker.line);
    } else terminal.scrollToBottom();
  } finally {
    marker?.dispose();
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
