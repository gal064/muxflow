import { describe, expect, it } from "vitest";
import {
  PANE_WATCHDOG_MAX_DELAY_MS,
  PaneDegradedWatchdog,
  type PaneDegradedReason,
  type PaneWatchdogTimers,
} from "./PaneDegradedWatchdog";

/** One pending timer at a time is the whole point, so the fake enforces it. */
function scriptedTimers() {
  const armed: Array<{ run: () => void; delay: number }> = [];
  let pending: { run: () => void; delay: number } | undefined;
  const timers: PaneWatchdogTimers = {
    setTimer(run, delay) {
      if (pending) throw new Error("watchdog armed a second timer over a live one");
      pending = { run, delay };
      armed.push(pending);
      return pending;
    },
    clearTimer(handle) {
      if (pending === handle) pending = undefined;
    },
  };
  return {
    timers,
    delays: () => armed.map((entry) => entry.delay),
    get armed() { return pending !== undefined; },
    fire() {
      const current = pending;
      if (!current) throw new Error("no watchdog timer is armed");
      pending = undefined;
      current.run();
    },
  };
}

describe("PaneDegradedWatchdog", () => {
  it("arms nothing until a pane is degraded", () => {
    const clock = scriptedTimers();
    const watchdog = new PaneDegradedWatchdog(() => undefined, clock.timers);
    watchdog.noteHealthy();
    watchdog.clear("revealFailed");
    expect(clock.armed).toBe(false);
    expect(watchdog.degraded).toBe(false);
  });

  it("backs off from two seconds to a thirty-second ceiling", () => {
    const clock = scriptedTimers();
    const retries: Array<[PaneDegradedReason, number]> = [];
    const watchdog = new PaneDegradedWatchdog((reason, attempt) => retries.push([reason, attempt]), clock.timers);
    watchdog.note("paneAwaitingSeed");
    for (let index = 0; index < 6; index += 1) clock.fire();
    expect(clock.delays()).toEqual([2_000, 4_000, 8_000, 16_000, PANE_WATCHDOG_MAX_DELAY_MS, PANE_WATCHDOG_MAX_DELAY_MS, PANE_WATCHDOG_MAX_DELAY_MS]);
    expect(retries.map(([, attempt]) => attempt)).toEqual([0, 1, 2, 3, 4, 5]);
    // The reported reason stays the most actionable one that still stands.
    expect(retries.every(([reason]) => reason === "paneAwaitingSeed")).toBe(true);
  });

  it("keeps waiting on the seed it asked for even after every other reason is withdrawn", () => {
    const clock = scriptedTimers();
    const watchdog = new PaneDegradedWatchdog(() => undefined, clock.timers);
    watchdog.note("hubConflictReseed");
    clock.fire();
    // Retrying reopens the hub's latch, which mirrors back as "not degraded any
    // more". The seed still has not arrived, so the bound must survive it.
    watchdog.clear("hubConflictReseed");
    expect(watchdog.degraded).toBe(true);
    expect(watchdog.primaryReason).toBe("seedRequested");
    expect(clock.armed).toBe(true);
  });

  it("disarms and restarts the backoff when the pane proves it is working", () => {
    const clock = scriptedTimers();
    const watchdog = new PaneDegradedWatchdog(() => undefined, clock.timers);
    watchdog.note("deferredOverflow");
    clock.fire();
    clock.fire();
    watchdog.noteHealthy();
    expect(clock.armed).toBe(false);
    expect(watchdog.attempts).toBe(0);
    watchdog.note("revealFailed");
    expect(clock.delays().at(-1)).toBe(2_000);
  });

  it("re-arms after a retry that throws, and never retries once stopped", () => {
    const clock = scriptedTimers();
    let attempts = 0;
    const watchdog = new PaneDegradedWatchdog(() => {
      attempts += 1;
      throw new Error("seed request could not be issued");
    }, clock.timers);
    watchdog.note("rendererReseed");
    expect(() => clock.fire()).toThrow("seed request could not be issued");
    expect(clock.armed).toBe(true);

    watchdog.stop();
    expect(clock.armed).toBe(false);
    watchdog.note("rendererReseed");
    expect(clock.armed).toBe(false);
    expect(attempts).toBe(1);
  });
});
