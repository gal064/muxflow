import { recordIncident } from "../../diagnostics/incidents";

/**
 * Attributes a slow paint to the mechanism that caused it.
 *
 * `inputLatencyStats` reports that the paint segment has a tail — a p50 of one
 * millisecond with rare samples of a quarter second — and a histogram cannot
 * say *why*. There are exactly four candidates behind an xterm write
 * completion that took three frames to fire, and each leaves a different mark:
 *
 * - **Backlog.** xterm queues chunks and parses them in time-sliced batches, so
 *   our callback waits behind whatever was already queued. Marked by a high
 *   `queuedWrites`/`queuedBytes` with no long task in the window.
 * - **Main-thread blockage.** One long task — a React render, a layout, a GC —
 *   owned the loop while our chunk waited. Marked by `longTasks`.
 * - **Atlas work.** The shared glyph atlas was invalidated mid-window and the
 *   texture had to be rebuilt. Marked by `atlasDelta > 0`.
 * - **A stopped frame clock.** macOS parks `requestAnimationFrame` for an
 *   occluded, minimized, or off-Space window while timers and IPC keep running,
 *   so anything paced by frames waits for the window to come back — which is a
 *   wait of seconds or minutes, not milliseconds, and it is invisible to all
 *   three fingerprints above. Marked by `msSinceLastFrame` far past a frame
 *   period, next to `visibility: "hidden"` or `focused: false`. This is the one
 *   cause where a huge `ms` with no long task and no atlas churn is the whole
 *   story, and without the heartbeat it reads exactly like unexplained backlog.
 *
 * All four fingerprints ride on one `render.paintSlow` record, so a single
 * journal line settles the question instead of a second measurement campaign.
 *
 * Always on, and priced for it: a paint under the threshold costs one
 * comparison, and a slow one costs a walk of a 32-entry ring. Rate limited per
 * pane, because the interesting fact about a pane painting slowly for a whole
 * minute is that it happened, not that it happened four hundred times — and the
 * count of what the limit swallowed rides on the next record so the silence
 * stays countable. Nothing here can throw into the write path.
 */

/**
 * How slow a write completion has to be before it is worth a journal line.
 *
 * Three frames at 60Hz. Below this a paint is inside the budget the user cannot
 * see past, and the tail this exists to explain starts an order of magnitude
 * above it.
 */
export const PAINT_SLOW_THRESHOLD_MS = 48;

/** How often one pane may contribute a slow-paint record. */
export const PAINT_SLOW_INCIDENT_INTERVAL_MS = 5_000;

/**
 * How many long tasks are kept. The window a paint asks about is tens of
 * milliseconds wide and a long task is at least fifty, so the last handful is
 * always enough; the bound is what keeps an hour of jank from growing an array.
 */
const LONG_TASK_RING_SIZE = 32;

interface LongTaskEntry {
  startTime: number;
  duration: number;
}

/**
 * What the long tasks intersecting one paint window add up to.
 *
 * `totalMs` and `maxMs` are the intersecting tasks' own durations, not the part
 * of them that fell inside the window: a 300ms task overlapping a 50ms paint is
 * reported as 300, because the size of the blockage is the finding.
 */
export interface LongTaskOverlap {
  count: number;
  totalMs: number;
  maxMs: number;
}

export interface PaintSample {
  paneId: string;
  /** How long the write completion took, in milliseconds. */
  ms: number;
  /** `performance.now()` at the moment the bytes were handed to the renderer. */
  startedAtMs: number;
  bytes: number;
  /** Our writes still in xterm's buffer, including this one. */
  queuedWrites: number;
  queuedBytes: number;
  /** How far the shared atlas's invalidation counter moved across the write. */
  atlasDelta: number | undefined;
}

interface PaneState {
  /** When this pane last emitted, on the same clock as `startedAtMs`. */
  lastRecordAtMs: number | undefined;
  /** Slow paints dropped by the rate limit since the last emitted record. */
  suppressed: number;
}

const ring: (LongTaskEntry | undefined)[] = new Array<LongTaskEntry | undefined>(LONG_TASK_RING_SIZE).fill(undefined);
let ringNext = 0;
let observer: { disconnect: () => void } | undefined;
/** Whether the long-task API is present and observing. */
let tracking = false;
/** Whether installation has been attempted; absence is permanent, so try once. */
let attempted = false;
const panes = new Map<string, PaneState>();
/** `performance.now()` at the most recent animation frame, or never a frame. */
let lastFrameAtMs: number | undefined;
let heartbeatFrame: number | undefined;
let heartbeatRunning = false;

function remember(entry: LongTaskEntry): void {
  ring[ringNext] = entry;
  ringNext = (ringNext + 1) % LONG_TASK_RING_SIZE;
}

/** The paint clock, with a fallback so a WebView without one cannot throw. */
function nowMs(): number {
  try {
    const clock = (globalThis as { performance?: { now?: () => number } }).performance;
    if (typeof clock?.now === "function") return clock.now();
  } catch {
    // Fall through to the wall clock.
  }
  return Date.now();
}

/**
 * Keeps one self-rescheduling frame in flight, purely to timestamp it.
 *
 * This is the only way to see a stopped frame clock from inside a write
 * completion: nothing else in the record moves when rAF is parked. One frame
 * callback storing one number is small enough to leave running for the life of
 * the app, and it is exactly as parked as the renderer it reports on — which is
 * the measurement.
 */
function startFrameClockHeartbeat(): void {
  if (heartbeatRunning) return;
  try {
    const request = (globalThis as {
      requestAnimationFrame?: (callback: (time: number) => void) => number;
    }).requestAnimationFrame;
    // Headless and test environments have no frame clock at all. Absent is not
    // stopped, so the field goes out undefined rather than as a huge gap.
    if (typeof request !== "function") return;
    heartbeatRunning = true;
    const tick = () => {
      if (!heartbeatRunning) return;
      lastFrameAtMs = nowMs();
      try {
        heartbeatFrame = request.call(globalThis, tick);
      } catch {
        heartbeatRunning = false;
        heartbeatFrame = undefined;
      }
    };
    lastFrameAtMs = nowMs();
    heartbeatFrame = request.call(globalThis, tick);
  } catch {
    heartbeatRunning = false;
    heartbeatFrame = undefined;
    lastFrameAtMs = undefined;
  }
}

function stopFrameClockHeartbeat(): void {
  heartbeatRunning = false;
  try {
    const cancel = (globalThis as { cancelAnimationFrame?: (handle: number) => void }).cancelAnimationFrame;
    if (heartbeatFrame !== undefined && typeof cancel === "function") cancel.call(globalThis, heartbeatFrame);
  } catch {
    // A frame that refuses to be cancelled still checks `heartbeatRunning`.
  }
  heartbeatFrame = undefined;
  lastFrameAtMs = undefined;
}

/**
 * How long since the frame clock last ticked, or `undefined` where there is no
 * frame clock to ask. Tens of milliseconds is a painting window; seconds is a
 * window macOS has stopped painting.
 */
export function msSinceLastFrame(): number | undefined {
  if (lastFrameAtMs === undefined) return undefined;
  const elapsed = nowMs() - lastFrameAtMs;
  if (!Number.isFinite(elapsed)) return undefined;
  return Math.round(Math.max(0, elapsed));
}

/** `document.visibilityState`, or undefined where there is no document. */
function readVisibility(): string | undefined {
  try {
    const state = (globalThis as { document?: { visibilityState?: string } }).document?.visibilityState;
    return typeof state === "string" ? state : undefined;
  } catch {
    return undefined;
  }
}

/** `document.hasFocus()`, or undefined where there is no document to ask. */
function readFocused(): boolean | undefined {
  try {
    const hasFocus = (globalThis as { document?: { hasFocus?: () => boolean } }).document?.hasFocus;
    if (typeof hasFocus !== "function") return undefined;
    const focused = hasFocus.call((globalThis as { document?: unknown }).document);
    return typeof focused === "boolean" ? focused : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Installs the long-task observer and the frame-clock heartbeat, once per app
 * launch.
 *
 * `longtask` is not in every WebView, and an absent API must cost the record
 * its long-task field and cost the app nothing — so construction and `observe`
 * are both guarded, and a failure leaves `tracking` false forever rather than
 * retrying on every pane mount. The heartbeat is armed alongside it and outside
 * that guard, because a WebView without long tasks can still stop painting.
 */
export function startLongTaskTracker(): void {
  if (attempted) return;
  attempted = true;
  startFrameClockHeartbeat();
  try {
    const observerCtor = (globalThis as {
      PerformanceObserver?: new (callback: (list: { getEntries: () => LongTaskEntry[] }) => void) => {
        observe: (options: { type: string; buffered: boolean }) => void;
        disconnect: () => void;
      };
    }).PerformanceObserver;
    if (typeof observerCtor !== "function") return;
    const created = new observerCtor((list) => {
      try {
        for (const entry of list.getEntries()) {
          remember({ startTime: entry.startTime, duration: entry.duration });
        }
      } catch {
        // A malformed entry list loses one batch of long tasks and nothing else.
      }
    });
    created.observe({ type: "longtask", buffered: false });
    observer = created;
    tracking = true;
  } catch {
    // No `longtask` support here. Every fingerprint field goes out undefined.
    observer = undefined;
    tracking = false;
  }
}

/** Stops observing and forgets what was seen. Test seam, and a clean teardown. */
export function disposeLongTaskTracker(): void {
  try {
    observer?.disconnect();
  } catch {
    // Nothing to do about an observer that refuses to stop.
  }
  stopFrameClockHeartbeat();
  observer = undefined;
  tracking = false;
  attempted = false;
  ring.fill(undefined);
  ringNext = 0;
}

/**
 * The long tasks intersecting `[startMs, endMs]`, or `undefined` when this
 * WebView has no long-task API to ask — which is not the same fact as "no long
 * task ran", and must not be reported as if it were.
 */
export function longTasksOverlapping(startMs: number, endMs: number): LongTaskOverlap | undefined {
  if (!tracking) return undefined;
  let count = 0;
  let totalMs = 0;
  let maxMs = 0;
  for (const entry of ring) {
    if (!entry) continue;
    if (entry.startTime >= endMs) continue;
    if (entry.startTime + entry.duration <= startMs) continue;
    count += 1;
    totalMs += entry.duration;
    if (entry.duration > maxMs) maxMs = entry.duration;
  }
  return { count, totalMs: Math.round(totalMs), maxMs: Math.round(maxMs) };
}

function stateFor(paneId: string): PaneState {
  const existing = panes.get(paneId);
  if (existing) return existing;
  const created: PaneState = { lastRecordAtMs: undefined, suppressed: 0 };
  panes.set(paneId, created);
  return created;
}

/**
 * Considers one write completion, and journals it when it was slow enough to
 * explain. Called on every live-output write, so the fast path is the one that
 * matters: a comparison and a return.
 */
export function notePaint(input: PaintSample): void {
  try {
    const { ms } = input;
    if (!Number.isFinite(ms) || ms < PAINT_SLOW_THRESHOLD_MS) return;
    const startedAtMs = Number.isFinite(input.startedAtMs) ? input.startedAtMs : 0;
    const state = stateFor(input.paneId);
    if (state.lastRecordAtMs !== undefined && startedAtMs - state.lastRecordAtMs < PAINT_SLOW_INCIDENT_INTERVAL_MS) {
      // Dropped, not deferred: holding the slowest sample of an interval would
      // mean a timer and a pending write to flush it, and the count of what was
      // dropped is what the next record needs to be read honestly.
      state.suppressed += 1;
      return;
    }
    const suppressed = state.suppressed;
    state.lastRecordAtMs = startedAtMs;
    state.suppressed = 0;
    recordIncident("render.paintSlow", {
      paneId: input.paneId,
      ms: Math.round(ms),
      bytes: input.bytes,
      queuedWrites: input.queuedWrites,
      queuedBytes: input.queuedBytes,
      atlasDelta: input.atlasDelta,
      longTasks: longTasksOverlapping(startedAtMs, startedAtMs + ms),
      // Whether this window was being painted at all while the write waited.
      visibility: readVisibility(),
      focused: readFocused(),
      msSinceLastFrame: msSinceLastFrame(),
      suppressed,
    });
  } catch {
    // A probe that throws into a write completion is worse than a missing line.
  }
}

/** Test seam. Feeds the ring without needing a real `longtask` observer. */
export function __noteLongTaskForTests(entry: LongTaskEntry): void {
  attempted = true;
  tracking = true;
  remember(entry);
}

/** Test seam. Resets the module to the state a fresh app launch starts in. */
export function __resetPaintTailProbeForTests(): void {
  disposeLongTaskTracker();
  panes.clear();
}
