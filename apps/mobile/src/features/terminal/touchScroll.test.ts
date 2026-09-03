import { describe, expect, it } from "vitest";

import { formatTouchScrollMetrics, TouchScrollController, type FrameScheduler, type TouchScrollMetrics } from "./touchScroll";

function harness() {
  let nextId = 1;
  let clockMs = 0;
  const callbacks = new Map<number, (timeMs: number) => void>();
  const scheduler: FrameScheduler = {
    request(callback) {
      const id = nextId++;
      callbacks.set(id, callback);
      return id;
    },
    cancel(id) {
      callbacks.delete(id);
    },
    now() {
      return clockMs;
    },
  };
  const calls: number[] = [];
  const metrics: TouchScrollMetrics[] = [];
  const controller = new TouchScrollController((rows) => calls.push(rows), scheduler, (entry) => metrics.push(entry));
  return {
    calls,
    controller,
    metrics,
    at(timeMs: number) {
      clockMs = timeMs;
    },
    frame(timeMs: number) {
      clockMs = timeMs;
      const pending = [...callbacks.values()];
      callbacks.clear();
      for (const callback of pending) callback(timeMs);
    },
    pendingFrames: () => callbacks.size,
  };
}

describe("TouchScrollController", () => {
  it("coalesces high-frequency moves into one xterm repaint per frame", () => {
    const h = harness();
    h.controller.start(160, 0);
    for (let index = 1; index <= 8; index += 1) h.controller.move(160 - index * 4, index * 2, 16);

    expect(h.calls).toEqual([]);
    expect(h.pendingFrames()).toBe(1);
    h.frame(16);
    expect(h.calls).toEqual([2]);
  });

  it("carries sub-row drag distance across frames", () => {
    const h = harness();
    h.controller.start(100, 0);
    h.controller.move(94, 16, 16);
    h.frame(16);
    expect(h.calls).toEqual([]);

    h.controller.move(88, 32, 16);
    h.frame(32);
    expect(h.calls).toEqual([]);

    h.controller.move(82, 48, 16);
    h.frame(48);
    expect(h.calls).toEqual([1]);
  });

  it("continues a quick drag with a decaying fling", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(160, 16, 16);
    h.frame(16);
    h.controller.move(120, 32, 16);
    h.controller.end(32);

    h.frame(48);
    h.frame(64);
    h.frame(80);
    expect(h.calls.length).toBeGreaterThanOrEqual(3);
    expect(h.calls.reduce((sum, rows) => sum + rows, 0)).toBeGreaterThan(5);
  });

  it("reports actual output timing when release arrives before the queued frame", () => {
    const h = harness();
    h.controller.start(160, 0);
    h.at(4);
    h.controller.move(128, 4, 16);
    h.at(8);
    h.controller.end(8);
    h.frame(16);
    for (let timeMs = 32; h.pendingFrames() > 0; timeMs += 16) h.frame(timeMs);

    expect(h.metrics[0]?.rowsWhilePressed).toBe(0);
    expect(h.metrics[0]?.rowsAfterRelease).toBeGreaterThan(0);
    expect(h.metrics[0]?.netRows).toBe(h.calls.reduce((sum, rows) => sum + rows, 0));
    expect(h.metrics[0]?.emittedDistanceRows).toBe(h.calls.reduce((sum, rows) => sum + Math.abs(rows), 0));
    expect((h.metrics[0]?.durationMs ?? 0) - (h.metrics[0]?.flingDurationMs ?? 0)).toBe(8);
    expect(h.metrics[0]?.scrollCalls).toBeLessThanOrEqual(h.metrics[0]?.frames ?? 0);
  });

  it("a new touch cancels an in-flight fling", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(120, 16, 16);
    h.controller.end(16);
    h.frame(32);
    const before = [...h.calls];

    h.controller.start(100, 40);
    h.frame(48);
    expect(h.calls).toEqual(before);
    expect(h.pendingFrames()).toBe(0);
    expect(h.metrics).toHaveLength(1);
    expect(h.metrics[0]?.interrupted).toBe(true);
  });

  it("does not fling after the finger pauses before release", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(120, 16, 16);
    h.frame(16);
    const afterDrag = [...h.calls];

    h.at(200);
    h.controller.end(200);
    h.frame(216);
    expect(h.calls).toEqual(afterDrag);
    expect(h.metrics[0]?.releaseVelocityRowsPerSecond).toBe(0);
    expect(h.metrics[0]?.rowsAfterRelease).toBe(0);
  });

  it("flings in the new direction after a quick reversal", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(160, 16, 16);
    h.frame(16);
    h.controller.move(120, 32, 16);
    h.frame(32);
    h.controller.move(136, 48, 16);
    h.frame(48);
    h.controller.end(48);
    h.frame(64);

    expect(h.calls.at(-1)).toBeLessThan(0);
    while (h.pendingFrames() > 0) h.frame(80 + h.calls.length * 16);
    expect(h.metrics[0]?.releaseVelocityRowsPerSecond).toBeLessThan(0);
  });

  it("decays rather than looping forever when event and frame clocks use different origins", () => {
    const h = harness();
    h.controller.start(200, 9_000);
    h.at(8);
    h.controller.move(120, 9_016, 16);
    h.at(9);
    h.controller.end(9_016);

    for (let frame = 1; frame <= 300 && h.pendingFrames() > 0; frame += 1) h.frame(frame * 16);
    expect(h.pendingFrames()).toBe(0);
    expect(h.metrics).toHaveLength(1);
    expect(h.metrics[0]?.durationMs).toBeGreaterThan(16);
    expect(h.metrics[0]?.durationMs).toBeLessThan(5_000);
    expect(h.metrics[0]?.flingDurationMs).toBeGreaterThan(0);
    expect(h.metrics[0]?.maxFrameWaitMs).toBeLessThanOrEqual(16);
  });

  it("records a real frame stall longer than one second", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(120, 16, 16);
    h.frame(1_100);
    h.controller.end(200); // stale release: settle without momentum
    h.frame(1_116);

    expect(h.metrics[0]?.slowFrames).toBeGreaterThanOrEqual(1);
    expect(h.metrics[0]?.maxFrameWaitMs).toBe(1_100);
  });

  it("ignores invalid cell measurements and cancel drops queued work", () => {
    const h = harness();
    h.controller.start(100, 0);
    expect(h.controller.move(80, 16, 0)).toBe(false);
    h.controller.move(80, 16, 16);
    h.controller.cancel();
    h.frame(32);
    expect(h.calls).toEqual([]);
  });

  it("emits one compact diagnostic summary when a gesture settles", () => {
    const h = harness();
    h.controller.start(160, 0);
    for (let index = 1; index <= 8; index += 1) {
      h.at(index * 2);
      h.controller.move(160 - index * 4, index * 2, 16);
    }
    h.frame(16);
    h.at(200);
    h.controller.end(200); // stale release: finish without a fling
    h.frame(216);

    expect(h.metrics).toEqual([expect.objectContaining({
      durationMs: 216,
      flingDurationMs: 16,
      moveEvents: 8,
      frames: 2,
      scrollCalls: 1,
      dragDistanceRows: 2,
      rowsWhilePressed: 2,
      rowsAfterRelease: 0,
      netRows: 2,
      emittedDistanceRows: 2,
      interrupted: false,
    })]);
    expect(formatTouchScrollMetrics(h.metrics[0]!)).toContain(
      "moves=8 frames=2 calls=1 dragDistanceRows=2 pressedRows=2 afterReleaseRows=0 netRows=2 emittedDistanceRows=2",
    );
  });
});
