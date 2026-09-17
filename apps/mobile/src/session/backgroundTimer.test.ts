import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createNativeBackgroundTimer, jsBackgroundTimer, type WakeClock } from "./backgroundTimer";

/** A stand-in for the native clock: records schedules, fires wakes on demand. */
function fakeClock(options: { rejectSchedule?: boolean } = {}) {
  const listeners = new Set<(token: string) => void>();
  const scheduled: Array<{ token: string; delayMs: number }> = [];
  const cancelled: string[] = [];
  const clock: WakeClock = {
    scheduleWake: vi.fn(async (token: string, delayMs: number) => {
      if (options.rejectSchedule) throw new Error("react context lost");
      scheduled.push({ token, delayMs });
    }),
    cancelWake: vi.fn(async (token: string) => {
      cancelled.push(token);
    }),
    addWakeListener: vi.fn((listener: (token: string) => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    }),
  };
  return {
    clock,
    scheduled,
    cancelled,
    wake: (token: string) => {
      for (const listener of [...listeners]) listener(token);
    },
    listenerCount: () => listeners.size,
  };
}

const flush = () => vi.advanceTimersByTimeAsync(0);

describe("native background timer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("schedules a wake per set, and runs the callback whose token comes back", async () => {
    const c = fakeClock();
    const timer = createNativeBackgroundTimer(c.clock, () => undefined);
    const fired: string[] = [];
    const a = timer.set(1000, () => fired.push("a"));
    const b = timer.set(2000, () => fired.push("b"));
    await flush();
    expect(a.token).not.toBe(b.token);
    expect(c.scheduled).toEqual([
      { token: a.token, delayMs: 1000 },
      { token: b.token, delayMs: 2000 },
    ]);
    // One native subscription serves every timer.
    expect(c.listenerCount()).toBe(1);

    c.wake(b.token);
    expect(fired).toEqual(["b"]);
    // A token fires once, and one nobody issued is ignored.
    c.wake(b.token);
    c.wake("nope");
    expect(fired).toEqual(["b"]);
    c.wake(a.token);
    expect(fired).toEqual(["b", "a"]);
    // Nothing to cancel: a JS timer is never involved on the native path.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fired).toEqual(["b", "a"]);
  });

  it("a cleared timer never fires, even when its wake arrives late", async () => {
    const c = fakeClock();
    const timer = createNativeBackgroundTimer(c.clock, () => undefined);
    const fired: string[] = [];
    const handle = timer.set(1000, () => fired.push("a"));
    timer.clear(handle);
    timer.clear(handle); // idempotent: the native cancel goes out once
    await flush();
    expect(c.cancelled).toEqual([handle.token]);
    c.wake(handle.token);
    expect(fired).toEqual([]);
  });

  it("falls back to setTimeout when the native schedule is refused, still honouring clear", async () => {
    const c = fakeClock({ rejectSchedule: true });
    const lines: string[] = [];
    const timer = createNativeBackgroundTimer(c.clock, (line) => lines.push(line));
    const fired: string[] = [];
    const a = timer.set(1000, () => fired.push("a"));
    const b = timer.set(1000, () => fired.push("b"));
    await flush();
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain("react context lost");
    timer.clear(b);
    await vi.advanceTimersByTimeAsync(999);
    expect(fired).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(fired).toEqual(["a"]);
    // The late native wake for an already-fired token is a no-op too.
    c.wake(a.token);
    expect(fired).toEqual(["a"]);
  });
});

describe("setTimeout background timer", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("fires after the delay and not at all once cleared", async () => {
    const fired: string[] = [];
    const a = jsBackgroundTimer.set(500, () => fired.push("a"));
    const b = jsBackgroundTimer.set(500, () => fired.push("b"));
    expect(a.token).not.toBe(b.token);
    jsBackgroundTimer.clear(b);
    jsBackgroundTimer.clear(b);
    await vi.advanceTimersByTimeAsync(499);
    expect(fired).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(fired).toEqual(["a"]);
    jsBackgroundTimer.clear(a); // already fired: no-op
  });
});
