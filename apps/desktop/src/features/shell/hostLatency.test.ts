import { beforeEach, describe, expect, it } from "vitest";
import { hostLatency, recordHostRoundTrip, resetHostLatency, STALE_AFTER_MS } from "./hostLatency";

describe("host latency readout", () => {
  beforeEach(() => resetHostLatency());

  it("has nothing to show until a real round-trip has happened", () => {
    expect(hostLatency()).toBeUndefined();
    recordHostRoundTrip(40, 1_000);
    expect(hostLatency(1_000)?.milliseconds).toBe(40);
  });

  it("smooths, so one slow action does not become the reported link speed", () => {
    recordHostRoundTrip(40, 1_000);
    recordHostRoundTrip(240, 1_100);
    expect(hostLatency(1_100)!.milliseconds).toBeCloseTo(100, 5);
  });

  it("expires rather than showing a number that no longer describes the link", () => {
    recordHostRoundTrip(40, 1_000);
    expect(hostLatency(1_000 + STALE_AFTER_MS)).toBeDefined();
    expect(hostLatency(1_001 + STALE_AFTER_MS)).toBeUndefined();
    // A reading that expired must not be smoothed into the next one.
    recordHostRoundTrip(200, 2_000 + STALE_AFTER_MS);
    expect(hostLatency(2_000 + STALE_AFTER_MS)!.milliseconds).toBe(200);
  });

  it("forgets everything when the bridge is replaced", () => {
    recordHostRoundTrip(40, 1_000);
    resetHostLatency();
    expect(hostLatency(1_000)).toBeUndefined();
  });

  it("ignores impossible samples instead of rendering them", () => {
    recordHostRoundTrip(Number.NaN, 1_000);
    recordHostRoundTrip(-5, 1_000);
    expect(hostLatency(1_000)).toBeUndefined();
  });
});
