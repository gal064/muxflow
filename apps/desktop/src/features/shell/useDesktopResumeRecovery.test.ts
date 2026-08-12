import { describe, expect, it } from "vitest";
import { RESUME_GAP_MS, ResumeGapDetector, ResumeTransitionDetector } from "./useDesktopResumeRecovery";

describe("desktop suspend/resume detection", () => {
  it("ignores ordinary timer and focus activity", () => {
    const detector = new ResumeGapDetector(1_000);
    expect(detector.observe(6_000)).toBe(false);
    expect(detector.observe(11_000)).toBe(false);
  });

  it("requests exactly one recovery after a suspended timer gap", () => {
    const detector = new ResumeGapDetector(1_000);
    expect(detector.observe(1_000 + RESUME_GAP_MS)).toBe(true);
    expect(detector.observe(1_000 + RESUME_GAP_MS + 1)).toBe(false);
    expect(detector.observe(1_000 + RESUME_GAP_MS * 2)).toBe(false);
    expect(detector.observe(1_000 + RESUME_GAP_MS * 3 + 1)).toBe(true);
  });

  it("ignores a non-finite clock sample without losing the previous observation", () => {
    const detector = new ResumeGapDetector(500);
    expect(detector.observe(Number.NaN)).toBe(false);
    expect(detector.observe(500 + RESUME_GAP_MS)).toBe(true);
  });

  it("ignores wall-clock corrections because samples are monotonic durations", () => {
    const detector = new ResumeGapDetector(10_000);
    expect(detector.observe(9_000)).toBe(false);
    expect(detector.observe(10_000)).toBe(false);
  });

  it("recovers after even a short hidden or offline transition", () => {
    const detector = new ResumeTransitionDetector(false, false);
    expect(detector.visibility(true)).toBe(false);
    expect(detector.visibility(false)).toBe(true);
    expect(detector.network(false)).toBe(false);
    expect(detector.network(true)).toBe(true);
    expect(detector.visibility(false)).toBe(false);
    expect(detector.network(true)).toBe(false);
  });
});
