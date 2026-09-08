import { describe, expect, it } from "vitest";

import { TouchScrollController, type FrameScheduler } from "./touchScroll";

function harness() {
  let nextId = 1;
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
  };
  const calls: number[] = [];
  const summaries: { durationMs: number; cancelled: boolean }[] = [];
  const controller = new TouchScrollController((rows) => calls.push(rows), scheduler, (summary) => summaries.push(summary));
  return {
    calls,
    summaries,
    controller,
    frame(timeMs: number) {
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
    expect(h.calls).toEqual([4]);
  });

  it("carries sub-row drag distance across frames", () => {
    const h = harness();
    h.controller.start(100, 0);
    h.controller.move(94, 16, 16);
    h.frame(16);
    expect(h.calls).toEqual([]);

    h.controller.move(88, 32, 16);
    h.frame(32);
    expect(h.calls).toEqual([1]);

    h.controller.move(82, 48, 16);
    h.frame(48);
    expect(h.calls).toEqual([1, 1]);
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

  it("flushes queued drag and momentum at most once per frame after release", () => {
    const h = harness();
    h.controller.start(160, 0);
    h.controller.move(128, 4, 16);
    h.controller.end(8);
    let frames = 1;
    h.frame(16);
    for (let timeMs = 32; h.pendingFrames() > 0; timeMs += 16) {
      frames += 1;
      h.frame(timeMs);
    }

    expect(h.calls.reduce((sum, rows) => sum + rows, 0)).toBeGreaterThan(4);
    expect(h.calls.length).toBeLessThanOrEqual(frames);
    expect(h.summaries).toEqual([{ durationMs: frames * 16, cancelled: false }]);
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
  });

  it("does not fling after the finger pauses before release", () => {
    const h = harness();
    h.controller.start(200, 0);
    h.controller.move(120, 16, 16);
    h.frame(16);
    const afterDrag = [...h.calls];

    h.controller.end(200);
    h.frame(216);
    expect(h.calls).toEqual(afterDrag);
    expect(h.pendingFrames()).toBe(0);
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
  });

  it("decays rather than looping forever when event and frame clocks use different origins", () => {
    const h = harness();
    h.controller.start(200, 9_000);
    h.controller.move(120, 9_016, 16);
    h.controller.end(9_016);

    for (let frame = 1; frame <= 300 && h.pendingFrames() > 0; frame += 1) h.frame(frame * 16);
    expect(h.pendingFrames()).toBe(0);
  });

  it("ignores invalid cell measurements and cancel drops queued work", () => {
    const h = harness();
    h.controller.start(100, 0);
    expect(h.controller.move(80, 16, 0)).toBe(false);
    h.controller.move(80, 16, 16);
    h.controller.cancel(20);
    h.frame(32);
    expect(h.calls).toEqual([]);
    expect(h.summaries).toEqual([{ durationMs: 20, cancelled: true }]);
  });
});
