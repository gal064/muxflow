import { describe, expect, it } from "vitest";
import {
  createLinkQualityMonitor,
  describeLinkQuality,
  LINK_QUALITY_CLEAR_MS,
  SLOW_LAG_ALONE_MS,
  SLOW_LAG_MIN_MS,
  SLOW_WINDOW_MS,
  UNSTABLE_WINDOW_MS,
} from "./linkQuality";

describe("link quality", () => {
  it("says nothing about a single drop, or two that are an hour apart", () => {
    const monitor = createLinkQualityMonitor();
    expect(monitor.noteLinkLost(0)).toBeUndefined();
    expect(monitor.noteLinkLost(UNSTABLE_WINDOW_MS + 1)).toBeUndefined();
    expect(monitor.state).toBeUndefined();
  });

  it("calls a link that dropped twice inside the window unstable, once", () => {
    const monitor = createLinkQualityMonitor();
    expect(monitor.noteLinkLost(1_000)).toBeUndefined();
    expect(monitor.noteLinkLost(20_000)).toEqual({
      kind: "degraded",
      state: "unstable",
      losses: 2,
      lagEvents: 0,
    });
    // The episode is already spoken for; further drops only extend it.
    expect(monitor.noteLinkLost(40_000)).toBeUndefined();
    expect(monitor.state).toBe("unstable");
  });

  it("calls two late echoes inside the window slow", () => {
    const monitor = createLinkQualityMonitor();
    expect(monitor.noteEchoLag(0, 1_000)).toBeUndefined();
    expect(monitor.noteEchoLag(SLOW_WINDOW_MS, 1_000)).toEqual({
      kind: "degraded",
      state: "slow",
      losses: 0,
      lagEvents: 2,
    });
    expect(monitor.noteEchoLag(SLOW_WINDOW_MS + 1_000, 1_000)).toBeUndefined();
  });

  it("climbs from slow to unstable, and never back down inside one episode", () => {
    const monitor = createLinkQualityMonitor();
    monitor.noteEchoLag(0, 1_000);
    expect(monitor.noteEchoLag(1_000, 1_000)).toMatchObject({ kind: "degraded", state: "slow" });
    monitor.noteLinkLost(2_000);
    expect(monitor.noteLinkLost(3_000)).toEqual({
      kind: "degraded",
      state: "unstable",
      losses: 2,
      lagEvents: 2,
    });
    // The lag records age out of their own minute; the episode stays unstable
    // rather than announcing itself a second time as merely slow.
    expect(monitor.noteLinkLost(4_000 + SLOW_WINDOW_MS)).toBeUndefined();
    expect(monitor.state).toBe("unstable");
  });

  it("clears only after a quiet window, and reports how long it stood", () => {
    const monitor = createLinkQualityMonitor();
    monitor.noteLinkLost(1_000);
    monitor.noteLinkLost(2_000);
    expect(monitor.poll(2_000 + LINK_QUALITY_CLEAR_MS - 1)).toBeUndefined();
    // A trigger inside the quiet window restarts it.
    monitor.noteLinkLost(2_000 + LINK_QUALITY_CLEAR_MS - 1);
    expect(monitor.poll(2_000 + LINK_QUALITY_CLEAR_MS + 1)).toBeUndefined();
    expect(monitor.poll(2_000 + 2 * LINK_QUALITY_CLEAR_MS)).toEqual({
      kind: "cleared",
      afterMs: 2_000 + 2 * LINK_QUALITY_CLEAR_MS - 2_000,
    });
    expect(monitor.state).toBeUndefined();
    // Cleared means cleared: the drops behind the last episode are spent.
    expect(monitor.poll(9_000_000)).toBeUndefined();
    expect(monitor.noteLinkLost(9_000_000)).toBeUndefined();
  });

  it("carries nothing across a reset to a different host", () => {
    const monitor = createLinkQualityMonitor();
    monitor.noteLinkLost(0);
    monitor.reset();
    expect(monitor.noteLinkLost(1_000)).toBeUndefined();
    expect(monitor.state).toBeUndefined();
  });

  it("names the host and the symptom in the user's own terms", () => {
    expect(describeLinkQuality("unstable", "build-01")).toBe(
      "Your connection to build-01 is unstable; Muxflow keeps losing the link and reconnecting.",
    );
    expect(describeLinkQuality("slow", "build-01")).toBe(
      "Your connection to build-01 is slow; the host is answering, but late.",
    );
  });
});

it("does not count an echo that is late by less than a network's worth", () => {
  const monitor = createLinkQualityMonitor();
  expect(monitor.noteEchoLag(0, SLOW_LAG_MIN_MS - 1)).toBeUndefined();
  expect(monitor.noteEchoLag(1_000, SLOW_LAG_MIN_MS - 1)).toBeUndefined();
  expect(monitor.state).toBeUndefined();
  expect(monitor.noteEchoLag(2_000, SLOW_LAG_MIN_MS)).toBeUndefined();
  expect(monitor.noteEchoLag(3_000, SLOW_LAG_MIN_MS)).toMatchObject({ kind: "degraded", state: "slow" });
});

it("calls the link slow on one late request, or one echo late by a network's worth", () => {
  const late = createLinkQualityMonitor();
  expect(late.noteLateRequest(0)).toMatchObject({ kind: "degraded", state: "slow" });
  const echo = createLinkQualityMonitor();
  expect(echo.noteEchoLag(0, SLOW_LAG_ALONE_MS)).toMatchObject({ kind: "degraded", state: "slow" });
  const mild = createLinkQualityMonitor();
  expect(mild.noteEchoLag(0, SLOW_LAG_ALONE_MS - 1)).toBeUndefined();
});
