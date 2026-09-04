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
  /** Monotonic clock used only for diagnostics. */
  now(): number;
}

export interface TouchScrollMetrics {
  durationMs: number;
  flingDurationMs: number;
  moveEvents: number;
  frames: number;
  scrollCalls: number;
  dragDistanceRows: number;
  rowsWhilePressed: number;
  rowsAfterRelease: number;
  netRows: number;
  emittedDistanceRows: number;
  releaseVelocityRowsPerSecond: number;
  slowFrames: number;
  maxFrameWaitMs: number;
  interrupted: boolean;
}

function oneDecimal(value: number): number {
  return Math.round(value * 10) / 10;
}

/** One compact, copyable line per gesture; no terminal content is included. */
export function formatTouchScrollMetrics(metrics: TouchScrollMetrics): string {
  return [
    "scroll.gesture",
    `durationMs=${oneDecimal(metrics.durationMs)}`,
    `flingMs=${oneDecimal(metrics.flingDurationMs)}`,
    `moves=${metrics.moveEvents}`,
    `frames=${metrics.frames}`,
    `calls=${metrics.scrollCalls}`,
    `dragDistanceRows=${oneDecimal(metrics.dragDistanceRows)}`,
    `pressedRows=${metrics.rowsWhilePressed}`,
    `afterReleaseRows=${metrics.rowsAfterRelease}`,
    `netRows=${metrics.netRows}`,
    `emittedDistanceRows=${metrics.emittedDistanceRows}`,
    `releaseRowsPerSec=${oneDecimal(metrics.releaseVelocityRowsPerSecond)}`,
    `slowFrames=${metrics.slowFrames}`,
    `maxFrameWaitMs=${oneDecimal(metrics.maxFrameWaitMs)}`,
    `interrupted=${metrics.interrupted}`,
  ].join(" ");
}

interface GestureMetrics {
  startedAtClockMs: number;
  releasedAtClockMs?: number;
  moveEvents: number;
  frames: number;
  scrollCalls: number;
  dragDistanceRows: number;
  rowsWhilePressed: number;
  rowsAfterRelease: number;
  netRows: number;
  emittedDistanceRows: number;
  releaseVelocityRowsPerSecond: number;
  slowFrames: number;
  maxFrameWaitMs: number;
}

const MAX_VELOCITY_ROWS_PER_MS = 0.25;
const FLING_STOP_ROWS_PER_MS = 0.0025;
const FLING_TIME_CONSTANT_MS = 325;
const VELOCITY_SAMPLE_MAX_AGE_MS = 80;
const VELOCITY_BLEND = 0.35;
const TOUCH_SCROLL_SENSITIVITY = 2;
const MAX_FRAME_MS = 34;
const SLOW_FRAME_WAIT_MS = 25;

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
  private frameRequestedAtClockMs: number | undefined;
  private gesture: GestureMetrics | undefined;

  constructor(
    private readonly scrollLines: (rows: number) => void,
    private readonly scheduler: FrameScheduler,
    private readonly onMetrics?: (metrics: TouchScrollMetrics) => void,
  ) {}

  start(y: number, timeMs: number): void {
    this.stopFrame();
    this.finishGesture(true);
    this.touchY = y;
    this.touchTimeMs = timeMs;
    this.pendingRows = 0;
    this.velocityRowsPerMs = 0;
    this.frameTimeMs = undefined;
    this.gesture = {
      startedAtClockMs: this.scheduler.now(),
      moveEvents: 0,
      frames: 0,
      scrollCalls: 0,
      dragDistanceRows: 0,
      rowsWhilePressed: 0,
      rowsAfterRelease: 0,
      netRows: 0,
      emittedDistanceRows: 0,
      releaseVelocityRowsPerSecond: 0,
      slowFrames: 0,
      maxFrameWaitMs: 0,
    };
  }

  /** Returns false when the move cannot be converted into terminal rows. */
  move(y: number, timeMs: number, cellHeight: number): boolean {
    if (this.touchY === undefined || this.touchTimeMs === undefined || !Number.isFinite(cellHeight) || cellHeight <= 0) return false;

    const fingerDeltaRows = (this.touchY - y) / cellHeight;
    const deltaRows = fingerDeltaRows * TOUCH_SCROLL_SENSITIVITY;
    const elapsedMs = timeMs - this.touchTimeMs;
    if (elapsedMs > 0 && elapsedMs <= VELOCITY_SAMPLE_MAX_AGE_MS) {
      const sampled = clamp(deltaRows / elapsedMs, -MAX_VELOCITY_ROWS_PER_MS, MAX_VELOCITY_ROWS_PER_MS);
      this.velocityRowsPerMs = sampled !== 0 && this.velocityRowsPerMs !== 0 && Math.sign(sampled) !== Math.sign(this.velocityRowsPerMs)
        ? sampled
        : this.velocityRowsPerMs * (1 - VELOCITY_BLEND) + sampled * VELOCITY_BLEND;
    } else if (elapsedMs > VELOCITY_SAMPLE_MAX_AGE_MS) {
      this.velocityRowsPerMs = 0;
    }

    if (this.gesture) {
      this.gesture.moveEvents += 1;
      this.gesture.dragDistanceRows += Math.abs(fingerDeltaRows);
    }
    this.touchY = y;
    this.touchTimeMs = timeMs;
    this.pendingRows += deltaRows;
    this.scheduleFrame();
    return true;
  }

  end(timeMs: number): void {
    if (this.touchY === undefined) return;
    if (this.touchTimeMs === undefined || timeMs - this.touchTimeMs > VELOCITY_SAMPLE_MAX_AGE_MS) this.velocityRowsPerMs = 0;
    if (this.gesture) {
      this.gesture.releasedAtClockMs = this.scheduler.now();
      this.gesture.releaseVelocityRowsPerSecond = this.velocityRowsPerMs * 1_000;
    }
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
    this.finishGesture(true);
  }

  private scheduleFrame(): void {
    if (this.frame !== undefined) return;
    this.frameRequestedAtClockMs = this.scheduler.now();
    this.frame = this.scheduler.request((timeMs) => this.onFrame(timeMs));
  }

  private onFrame(timeMs: number): void {
    this.frame = undefined;
    const dragging = this.touchY !== undefined;
    const clockNow = this.scheduler.now();
    const requestedAtClockMs = this.frameRequestedAtClockMs ?? clockNow;
    const rawFrameWaitMs = clockNow - requestedAtClockMs;
    const frameWaitMs = Number.isFinite(rawFrameWaitMs) && rawFrameWaitMs >= 0 ? rawFrameWaitMs : 0;
    if (this.gesture) {
      this.gesture.frames += 1;
      this.gesture.maxFrameWaitMs = Math.max(this.gesture.maxFrameWaitMs, frameWaitMs);
      if (frameWaitMs > SLOW_FRAME_WAIT_MS) this.gesture.slowFrames += 1;
    }
    this.frameRequestedAtClockMs = undefined;

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
      if (this.gesture) {
        if (dragging) this.gesture.rowsWhilePressed += Math.abs(rows);
        else this.gesture.rowsAfterRelease += Math.abs(rows);
        this.gesture.netRows += rows;
        this.gesture.emittedDistanceRows += Math.abs(rows);
        this.gesture.scrollCalls += 1;
      }
      this.scrollLines(rows);
    }

    if (!dragging && Math.abs(this.velocityRowsPerMs) >= FLING_STOP_ROWS_PER_MS) this.scheduleFrame();
    else if (!dragging) this.finishGesture(false);
  }

  private stopFrame(): void {
    if (this.frame !== undefined) this.scheduler.cancel(this.frame);
    this.frame = undefined;
    this.frameRequestedAtClockMs = undefined;
  }

  private finishGesture(interrupted: boolean): void {
    const gesture = this.gesture;
    if (!gesture) return;
    this.gesture = undefined;
    const finishedAtClockMs = this.scheduler.now();
    const durationMs = Math.max(0, finishedAtClockMs - gesture.startedAtClockMs);
    const flingDurationMs = gesture.releasedAtClockMs === undefined
      ? 0
      : Math.max(0, finishedAtClockMs - gesture.releasedAtClockMs);
    this.onMetrics?.({
      durationMs,
      flingDurationMs,
      moveEvents: gesture.moveEvents,
      frames: gesture.frames,
      scrollCalls: gesture.scrollCalls,
      dragDistanceRows: gesture.dragDistanceRows,
      rowsWhilePressed: gesture.rowsWhilePressed,
      rowsAfterRelease: gesture.rowsAfterRelease,
      netRows: gesture.netRows,
      emittedDistanceRows: gesture.emittedDistanceRows,
      releaseVelocityRowsPerSecond: gesture.releaseVelocityRowsPerSecond,
      slowFrames: gesture.slowFrames,
      maxFrameWaitMs: gesture.maxFrameWaitMs,
      interrupted,
    });
  }
}
