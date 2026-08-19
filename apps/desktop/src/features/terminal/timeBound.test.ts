import { afterEach, describe, expect, it, vi } from "vitest";
import { awaitWithin, settleWithin } from "./timeBound";

afterEach(() => {
  vi.useRealTimers();
});

describe("settleWithin", () => {
  it("resolves with the work's own value and cancels its bound", async () => {
    vi.useFakeTimers();
    let timedOut = false;
    const bounded = settleWithin(Promise.resolve("real"), 1_000, () => {
      timedOut = true;
      return "fallback";
    });
    await expect(bounded).resolves.toBe("real");
    // Nothing is left armed: a bounded wait that won its race costs nothing
    // afterwards, and the fallback must not run late.
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(timedOut).toBe(false);
  });

  it("answers from the fallback when the work never settles", async () => {
    vi.useFakeTimers();
    const bounded = settleWithin(new Promise<string>(() => undefined), 1_000, () => "fallback");
    const settled = vi.fn();
    void bounded.then(settled);
    await vi.advanceTimersByTimeAsync(999);
    expect(settled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(settled).toHaveBeenCalledWith("fallback");
  });

  it("propagates a rejection rather than substituting the fallback", async () => {
    await expect(settleWithin(Promise.reject(new Error("drain failed")), 1_000, () => "fallback"))
      .rejects.toThrow("drain failed");
  });

  it("rejects when the fallback itself throws instead of leaving it unhandled", async () => {
    vi.useFakeTimers();
    const bounded = settleWithin(new Promise<string>(() => undefined), 10, () => {
      throw new Error("serialize failed");
    });
    const rejected = expect(bounded).rejects.toThrow("serialize failed");
    await vi.advanceTimersByTimeAsync(10);
    await rejected;
  });
});

describe("awaitWithin", () => {
  it("treats a rejection as a settlement, because the wait was for the ordering", async () => {
    await expect(awaitWithin(Promise.reject(new Error("hide failed")), 1_000)).resolves.toBe("settled");
    await expect(awaitWithin(Promise.resolve(), 1_000)).resolves.toBe("settled");
  });

  it("reports a wedged promise so the caller can stop serializing behind it", async () => {
    vi.useFakeTimers();
    const outcome = awaitWithin(new Promise(() => undefined), 2_000);
    await vi.advanceTimersByTimeAsync(2_000);
    await expect(outcome).resolves.toBe("timeout");
  });
});
