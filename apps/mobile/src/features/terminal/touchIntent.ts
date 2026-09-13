export const TERMINAL_LONG_PRESS_MS = 450;
export const TERMINAL_TOUCH_SLOP_PX = 8;

export interface TouchPoint {
  x: number;
  y: number;
}

export interface OneBasedTerminalRange {
  start: { x: number; y: number };
  end: { x: number; y: number };
}

/** Converts xterm's one-based, inclusive link range to `Terminal.select` arguments. */
export function selectionForTerminalRange(
  range: OneBasedTerminalRange,
  columns: number,
): { column: number; row: number; length: number } | undefined {
  if (columns < 1 || range.start.x < 1 || range.start.y < 1 || range.end.x < 1 || range.end.y < 1) return undefined;
  const start = (range.start.y - 1) * columns + range.start.x - 1;
  const end = (range.end.y - 1) * columns + range.end.x;
  if (end <= start) return undefined;
  return { column: start % columns, row: Math.floor(start / columns), length: end - start };
}

export function selectionEdgeScrollDirection(clientY: number, top: number, bottom: number, edgePx: number): -1 | 0 | 1 {
  if (clientY < top + edgePx) return -1;
  if (clientY > bottom - edgePx) return 1;
  return 0;
}

export type TouchMoveDecision = "pending" | "startScroll" | "scroll" | "selection" | "none";
export type TouchEndDecision = "tap" | "scroll" | "selection" | "none";

/** Pure gesture arbitration for the terminal's mutually exclusive tap, scroll and selection paths. */
export class TerminalTouchIntent {
  private state: "idle" | "pending" | "scroll" | "selection" = "idle";
  private origin: TouchPoint | undefined;

  start(point: TouchPoint): void {
    this.origin = point;
    this.state = "pending";
  }

  move(point: TouchPoint): TouchMoveDecision {
    if (this.state === "selection") return "selection";
    if (this.state === "scroll") return "scroll";
    if (this.state !== "pending" || !this.origin) return "none";
    const distance = Math.hypot(point.x - this.origin.x, point.y - this.origin.y);
    if (distance <= TERMINAL_TOUCH_SLOP_PX) return "pending";
    this.state = "scroll";
    return "startScroll";
  }

  longPress(): boolean {
    if (this.state !== "pending") return false;
    this.state = "selection";
    return true;
  }

  end(): TouchEndDecision {
    const decision: TouchEndDecision = this.state === "pending"
      ? "tap"
      : this.state === "scroll"
        ? "scroll"
        : this.state === "selection"
          ? "selection"
          : "none";
    this.reset();
    return decision;
  }

  cancel(): TouchEndDecision {
    const decision = this.end();
    return decision;
  }

  reset(): void {
    this.state = "idle";
    this.origin = undefined;
  }
}
