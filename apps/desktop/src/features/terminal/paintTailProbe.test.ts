import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  __noteLongTaskForTests,
  __resetPaintTailProbeForTests,
  longTasksOverlapping,
  msSinceLastFrame,
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

interface DocumentStub {
  visibilityState?: string;
  hasFocus?: () => boolean;
}

/** Swaps globals for the duration of one test, restoring absences as absences. */
function withGlobals(patch: Record<string, unknown>, body: () => void): void {
  const scope = globalThis as Record<string, unknown>;
  const saved = new Map<string, { present: boolean; value: unknown }>();
  for (const key of Object.keys(patch)) {
    saved.set(key, { present: key in scope, value: scope[key] });
    if (patch[key] === undefined) delete scope[key];
    else scope[key] = patch[key];
  }
  try {
    body();
  } finally {
    for (const [key, previous] of saved) {
      if (previous.present) scope[key] = previous.value;
      else delete scope[key];
    }
  }
}

/** A frame clock nobody advances, plus the window state that comes with it. */
function withFrameClock(
  document: DocumentStub | undefined,
  body: (frames: Array<(time: number) => void>) => void,
): void {
  const frames: Array<(time: number) => void> = [];
  withGlobals({
    requestAnimationFrame: (callback: (time: number) => void) => frames.push(callback),
    cancelAnimationFrame: () => undefined,
    document,
  }, () => body(frames));
}

describe("the stopped-frame-clock fingerprint", () => {
  it("reports how long the frame clock has been parked, and what the window was doing", () => {
    withFrameClock({ visibilityState: "hidden", hasFocus: () => false }, (frames) => {
      const now = vi.spyOn(performance, "now");
      try {
        now.mockReturnValue(1_000);
        startLongTaskTracker();
        // One frame in flight, and its timestamp is the heartbeat.
        expect(frames).toHaveLength(1);
        now.mockReturnValue(1_016);
        expect(msSinceLastFrame()).toBe(16);

        now.mockReturnValue(1_016);
        frames.shift()!(1_016);
        expect(frames).toHaveLength(1);
        // Nothing fires the re-armed frame: this is an occluded window, and the
        // gap is the only field on the record that grows with it.
        now.mockReturnValue(9_000);
        expect(msSinceLastFrame()).toBe(7_984);

        notePaint(paint({ startedAtMs: 0, ms: 7_900 }));
        expect(slowPaints()[0].detail).toMatchObject({
          visibility: "hidden",
          focused: false,
          msSinceLastFrame: 7_984,
        });
      } finally {
        now.mockRestore();
      }
    });
  });

  it("separates a painting window from a parked one on the same record", () => {
    withFrameClock({ visibilityState: "visible", hasFocus: () => true }, (frames) => {
      const now = vi.spyOn(performance, "now");
      try {
        now.mockReturnValue(500);
        startLongTaskTracker();
        now.mockReturnValue(508);
        frames.shift()!(508);
        now.mockReturnValue(512);
        notePaint(paint({ startedAtMs: 0 }));
        // A live frame clock: the slow paint has to be explained by one of the
        // other three fingerprints.
        expect(slowPaints()[0].detail).toMatchObject({
          visibility: "visible",
          focused: true,
          msSinceLastFrame: 4,
        });
      } finally {
        now.mockRestore();
      }
    });
  });

  it("stops the heartbeat on teardown, and a stale frame cannot restart it", () => {
    withFrameClock(undefined, (frames) => {
      startLongTaskTracker();
      const stale = frames.shift();
      expect(stale).toBeTypeOf("function");
      __resetPaintTailProbeForTests();
      stale?.(0);
      expect(frames).toHaveLength(0);
      expect(msSinceLastFrame()).toBeUndefined();
    });
  });
});

describe("an environment with no frame clock", () => {
  it("reports no gap rather than an infinite one, and does not throw", () => {
    withGlobals({ requestAnimationFrame: undefined, cancelAnimationFrame: undefined, document: undefined }, () => {
      expect(() => startLongTaskTracker()).not.toThrow();
      expect(msSinceLastFrame()).toBeUndefined();
      notePaint(paint());
      const detail = slowPaints()[0].detail ?? {};
      // Present and undefined: absent is not the same fact as "never parked".
      expect("msSinceLastFrame" in detail).toBe(true);
      expect(detail.msSinceLastFrame).toBeUndefined();
      expect(detail.visibility).toBeUndefined();
      expect(detail.focused).toBeUndefined();
    });
  });

  it("survives a frame clock that refuses to schedule", () => {
    withGlobals({
      requestAnimationFrame: () => {
        throw new Error("no frames for an occluded window");
      },
    }, () => {
      expect(() => startLongTaskTracker()).not.toThrow();
      expect(msSinceLastFrame()).toBeUndefined();
      expect(() => notePaint(paint())).not.toThrow();
      expect(slowPaints()).toHaveLength(1);
    });
  });

  it("tolerates a document that throws when asked what it is doing", () => {
    withGlobals({
      document: {
        get visibilityState(): string {
          throw new Error("detached");
        },
        hasFocus: () => {
          throw new Error("detached");
        },
      },
    }, () => {
      expect(() => notePaint(paint())).not.toThrow();
      const detail = slowPaints()[0].detail ?? {};
      expect(detail.visibility).toBeUndefined();
      expect(detail.focused).toBeUndefined();
    });
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
