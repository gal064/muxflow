// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PREVIEW_DEBOUNCE_MS, PREVIEW_IDLE_TIMEOUT_MS, scheduleSanitizedPreview } from "./markdownPreview";

describe("scheduleSanitizedPreview", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => {
    vi.useRealTimers();
    Reflect.deleteProperty(globalThis, "requestIdleCallback");
    Reflect.deleteProperty(globalThis, "cancelIdleCallback");
  });

  it("waits for typing to stop before doing any work", () => {
    const work = vi.fn();
    scheduleSanitizedPreview(work);
    vi.advanceTimersByTime(PREVIEW_DEBOUNCE_MS - 1);
    expect(work).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    vi.advanceTimersByTime(1);
    expect(work).toHaveBeenCalledTimes(1);
  });

  it("does nothing at all for a keystroke that is superseded", () => {
    const first = vi.fn();
    const cancel = scheduleSanitizedPreview(first);
    vi.advanceTimersByTime(PREVIEW_DEBOUNCE_MS - 1);
    cancel();
    const second = vi.fn();
    scheduleSanitizedPreview(second);
    vi.advanceTimersByTime(PREVIEW_DEBOUNCE_MS + 1);
    expect(first, "a superseded sanitize still ran").not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("hands the work to an idle moment where the platform offers one", () => {
    const idle: Array<{ work: () => void; options?: { timeout?: number } }> = [];
    Object.defineProperty(globalThis, "requestIdleCallback", {
      configurable: true,
      value: (work: () => void, options?: { timeout?: number }) => {
        idle.push({ work, options });
        return idle.length;
      },
    });
    const work = vi.fn();
    scheduleSanitizedPreview(work);
    vi.advanceTimersByTime(PREVIEW_DEBOUNCE_MS);
    expect(work, "the sanitize ran on the timer rather than at idle").not.toHaveBeenCalled();
    // The deadline is what stops a permanently busy page from never publishing.
    expect(idle[0].options?.timeout).toBe(PREVIEW_IDLE_TIMEOUT_MS);
    idle[0].work();
    expect(work).toHaveBeenCalledTimes(1);
  });

  it("withdraws an idle request that is cancelled before it runs", () => {
    const cancelled: number[] = [];
    Object.defineProperty(globalThis, "requestIdleCallback", { configurable: true, value: () => 7 });
    Object.defineProperty(globalThis, "cancelIdleCallback", {
      configurable: true,
      value: (handle: number) => cancelled.push(handle),
    });
    const cancel = scheduleSanitizedPreview(vi.fn());
    vi.advanceTimersByTime(PREVIEW_DEBOUNCE_MS);
    cancel();
    expect(cancelled).toEqual([7]);
  });
});
