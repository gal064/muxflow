/**
 * Frame-batched, kinetic scrolling for the terminal WebView.
 *
 * xterm's `scrollLines` repaints the viewport. Android can deliver more than
 * one touchmove per display frame, so calling it directly from every event
 * performs work the user can never see. This controller accumulates drag
 * distance and emits at most one scroll call per animation frame. A short,
 * decaying fling restores the momentum users expect from a native list.
 */

export interface FrameScheduler {
  request(callback: (timeMs: number) => void): number;
  cancel(id: number): void;
}

const MAX_VELOCITY_ROWS_PER_MS = 0.25;
const FLING_STOP_ROWS_PER_MS = 0.0025;
const FLING_TIME_CONSTANT_MS = 325;
const VELOCITY_SAMPLE_MAX_AGE_MS = 80;
const VELOCITY_BLEND = 0.35;
const MAX_FRAME_MS = 34;

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}

export class TouchScrollController {
  private touchY: number | undefined;
  private touchTimeMs: number | undefined;
  private pendingRows = 0;
  private velocityRowsPerMs = 0;
  private frame: number | undefined;
  private frameTimeMs: number | undefined;

  constructor(
    private readonly scrollLines: (rows: number) => void,
    private readonly scheduler: FrameScheduler,
  ) {}

  start(y: number, timeMs: number): void {
    this.stopFrame();
    this.touchY = y;
    this.touchTimeMs = timeMs;
    this.pendingRows = 0;
    this.velocityRowsPerMs = 0;
    this.frameTimeMs = undefined;
  }

  /** Returns false when the move cannot be converted into terminal rows. */
  move(y: number, timeMs: number, cellHeight: number): boolean {
    if (this.touchY === undefined || this.touchTimeMs === undefined || !Number.isFinite(cellHeight) || cellHeight <= 0) return false;

    const deltaRows = (this.touchY - y) / cellHeight;
    const elapsedMs = timeMs - this.touchTimeMs;
    if (elapsedMs > 0 && elapsedMs <= VELOCITY_SAMPLE_MAX_AGE_MS) {
      const sampled = clamp(deltaRows / elapsedMs, -MAX_VELOCITY_ROWS_PER_MS, MAX_VELOCITY_ROWS_PER_MS);
      this.velocityRowsPerMs = this.velocityRowsPerMs * (1 - VELOCITY_BLEND) + sampled * VELOCITY_BLEND;
    } else if (elapsedMs > VELOCITY_SAMPLE_MAX_AGE_MS) {
      this.velocityRowsPerMs = 0;
    }

    this.touchY = y;
    this.touchTimeMs = timeMs;
    this.pendingRows += deltaRows;
    this.scheduleFrame();
    return true;
  }

  end(timeMs: number): void {
    if (this.touchY === undefined) return;
    this.touchY = undefined;
    this.touchTimeMs = undefined;
    this.frameTimeMs = timeMs;
    this.scheduleFrame();
  }

  cancel(): void {
    this.stopFrame();
    this.touchY = undefined;
    this.touchTimeMs = undefined;
    this.pendingRows = 0;
    this.velocityRowsPerMs = 0;
    this.frameTimeMs = undefined;
  }

  private scheduleFrame(): void {
    if (this.frame !== undefined) return;
    this.frame = this.scheduler.request((timeMs) => this.onFrame(timeMs));
  }

  private onFrame(timeMs: number): void {
    this.frame = undefined;
    const dragging = this.touchY !== undefined;

    if (!dragging && Math.abs(this.velocityRowsPerMs) >= FLING_STOP_ROWS_PER_MS) {
      const rawElapsedMs = timeMs - (this.frameTimeMs ?? timeMs);
      // Event.timeStamp and rAF normally share a time origin. Treat an old
      // Android WebView that reports different origins as one display frame;
      // zero elapsed time would otherwise keep scheduling an immortal fling.
      const elapsedMs = clamp(Number.isFinite(rawElapsedMs) && rawElapsedMs > 0 ? rawElapsedMs : 16, 1, MAX_FRAME_MS);
      this.pendingRows += this.velocityRowsPerMs * elapsedMs;
      this.velocityRowsPerMs *= Math.exp(-elapsedMs / FLING_TIME_CONSTANT_MS);
      this.frameTimeMs = timeMs;
    }

    const rows = Math.trunc(this.pendingRows);
    if (rows !== 0) {
      this.pendingRows -= rows;
      this.scrollLines(rows);
    }

    if (!dragging && Math.abs(this.velocityRowsPerMs) >= FLING_STOP_ROWS_PER_MS) this.scheduleFrame();
  }

  private stopFrame(): void {
    if (this.frame !== undefined) this.scheduler.cancel(this.frame);
    this.frame = undefined;
  }
}
