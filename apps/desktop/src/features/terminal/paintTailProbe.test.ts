import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  __noteLongTaskForTests,
  __resetPaintTailProbeForTests,
  longTasksOverlapping,
  notePaint,
  PAINT_SLOW_INCIDENT_INTERVAL_MS,
  PAINT_SLOW_THRESHOLD_MS,
  startLongTaskTracker,
  type PaintSample,
} from "./paintTailProbe";

const recorded: { kind: string; detail?: Record<string, unknown> }[] = [];

vi.mock("../../diagnostics/incidents", () => ({
  recordIncident: (kind: string, detail?: Record<string, unknown>) => {
    recorded.push({ kind, detail });
  },
}));

/** A paint of the given cost, with the fingerprints a caller would supply. */
function paint(overrides: Partial<PaintSample> = {}): PaintSample {
  return {
    paneId: "pane-1",
    ms: 100,
    startedAtMs: 1_000,
    bytes: 4_096,
    queuedWrites: 1,
    queuedBytes: 4_096,
    atlasDelta: 0,
    ...overrides,
  };
}

const slowPaints = () => recorded.filter((r) => r.kind === "render.paintSlow");

beforeEach(() => {
  recorded.length = 0;
  __resetPaintTailProbeForTests();
});

afterEach(() => {
  __resetPaintTailProbeForTests();
});

describe("the slow-paint threshold", () => {
  it("keeps a paint inside the frame budget out of the journal", () => {
    notePaint(paint({ ms: PAINT_SLOW_THRESHOLD_MS - 1 }));
    expect(slowPaints()).toHaveLength(0);
  });

  it("records a paint at the threshold with every fingerprint it was given", () => {
    notePaint(paint({ ms: PAINT_SLOW_THRESHOLD_MS, queuedWrites: 7, queuedBytes: 90_000, atlasDelta: 2 }));
    expect(slowPaints()).toHaveLength(1);
    expect(slowPaints()[0].detail).toMatchObject({
      paneId: "pane-1",
      ms: PAINT_SLOW_THRESHOLD_MS,
      bytes: 4_096,
      queuedWrites: 7,
      queuedBytes: 90_000,
      atlasDelta: 2,
      suppressed: 0,
    });
  });

  it("rounds the duration, and says nothing about a duration that is not a number", () => {
    notePaint(paint({ ms: 123.6 }));
    expect(slowPaints()[0].detail).toMatchObject({ ms: 124 });
    notePaint(paint({ paneId: "pane-2", ms: Number.NaN }));
    expect(slowPaints()).toHaveLength(1);
  });
});

describe("the per-pane rate limit", () => {
  it("emits once per interval and counts what it swallowed onto the next record", () => {
    notePaint(paint({ startedAtMs: 0 }));
    expect(slowPaints()).toHaveLength(1);
    expect(slowPaints()[0].detail).toMatchObject({ suppressed: 0 });

    // Three more slow paints inside the interval: dropped, not queued, but
    // counted — the silence has to be readable in the next line.
    notePaint(paint({ startedAtMs: 1_000 }));
    notePaint(paint({ startedAtMs: 2_000 }));
    notePaint(paint({ startedAtMs: PAINT_SLOW_INCIDENT_INTERVAL_MS - 1 }));
    expect(slowPaints()).toHaveLength(1);

    notePaint(paint({ startedAtMs: PAINT_SLOW_INCIDENT_INTERVAL_MS }));
    expect(slowPaints()).toHaveLength(2);
    expect(slowPaints()[1].detail).toMatchObject({ suppressed: 3 });

    // And the counter resets with the record that carried it.
    notePaint(paint({ startedAtMs: PAINT_SLOW_INCIDENT_INTERVAL_MS * 2 }));
    expect(slowPaints()[2].detail).toMatchObject({ suppressed: 0 });
  });

  it("is charged per pane, so a busy pane cannot silence a quiet one", () => {
    notePaint(paint({ paneId: "a", startedAtMs: 0 }));
    notePaint(paint({ paneId: "a", startedAtMs: 10 }));
    notePaint(paint({ paneId: "b", startedAtMs: 10 }));
    expect(slowPaints().map((r) => r.detail?.paneId)).toEqual(["a", "b"]);
  });
});

describe("the long-task overlap", () => {
  it("counts only the tasks intersecting the paint window", () => {
    __noteLongTaskForTests({ startTime: 0, duration: 60 }); // ends before
    __noteLongTaskForTests({ startTime: 300, duration: 60 }); // starts after
    __noteLongTaskForTests({ startTime: 90, duration: 80 }); // straddles the start
    __noteLongTaskForTests({ startTime: 150, duration: 200 }); // straddles the end
    expect(longTasksOverlapping(100, 200)).toEqual({ count: 2, totalMs: 280, maxMs: 200 });
  });

  it("treats a task touching an edge as outside the window", () => {
    __noteLongTaskForTests({ startTime: 40, duration: 60 }); // ends exactly at 100
    __noteLongTaskForTests({ startTime: 200, duration: 60 }); // starts exactly at 200
    expect(longTasksOverlapping(100, 200)).toEqual({ count: 0, totalMs: 0, maxMs: 0 });
  });

  it("keeps only the most recent tasks, so an hour of jank cannot grow an array", () => {
    for (let i = 0; i < 50; i++) __noteLongTaskForTests({ startTime: i * 100, duration: 60 });
    // The window over the oldest task has been overwritten; the newest is kept.
    expect(longTasksOverlapping(0, 100)).toEqual({ count: 0, totalMs: 0, maxMs: 0 });
    expect(longTasksOverlapping(4_900, 5_000)).toMatchObject({ count: 1 });
  });

  it("rides on the record, computed over that paint's own window", () => {
    __noteLongTaskForTests({ startTime: 1_010, duration: 120 });
    notePaint(paint({ startedAtMs: 1_000, ms: 100 }));
    expect(slowPaints()[0].detail).toMatchObject({ longTasks: { count: 1, totalMs: 120, maxMs: 120 } });
  });
});

describe("a WebView with no long-task API", () => {
  it("reports the field as undefined rather than as an absence of long tasks", () => {
    expect(longTasksOverlapping(0, 100)).toBeUndefined();
    notePaint(paint());
    const detail = slowPaints()[0].detail ?? {};
    expect(detail.longTasks).toBeUndefined();
    expect("longTasks" in detail).toBe(true);
  });

  it("does not throw when the constructor is missing or refuses to observe", () => {
    const original = (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver;
    try {
      delete (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver;
      expect(() => startLongTaskTracker()).not.toThrow();
      expect(longTasksOverlapping(0, 100)).toBeUndefined();

      __resetPaintTailProbeForTests();
      (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver = class {
        observe(): void {
          throw new Error("longtask is not a supported entry type");
        }
        disconnect(): void {}
      };
      expect(() => startLongTaskTracker()).not.toThrow();
      expect(longTasksOverlapping(0, 100)).toBeUndefined();
    } finally {
      if (original === undefined) delete (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver;
      else (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver = original;
    }
  });

  it("observes long tasks when the API is there, and installs only once", () => {
    const original = (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver;
    const observed: unknown[] = [];
    try {
      (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver = class {
        constructor(private readonly callback: (list: { getEntries: () => unknown[] }) => void) {}
        observe(options: unknown): void {
          observed.push(options);
          this.callback({ getEntries: () => [{ startTime: 1_010, duration: 120 }] });
        }
        disconnect(): void {}
      };
      startLongTaskTracker();
      startLongTaskTracker();
      expect(observed).toEqual([{ type: "longtask", buffered: false }]);
      expect(longTasksOverlapping(1_000, 1_100)).toEqual({ count: 1, totalMs: 120, maxMs: 120 });
    } finally {
      if (original === undefined) delete (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver;
      else (globalThis as { PerformanceObserver?: unknown }).PerformanceObserver = original;
    }
  });
});
