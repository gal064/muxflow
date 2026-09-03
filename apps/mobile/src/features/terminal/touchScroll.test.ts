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
  const controller = new TouchScrollController((rows) => calls.push(rows), scheduler);
  return {
    calls,
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
    h.controller.cancel();
    h.frame(32);
    expect(h.calls).toEqual([]);
  });
});
