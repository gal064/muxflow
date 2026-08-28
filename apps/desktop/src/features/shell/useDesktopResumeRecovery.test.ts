import { describe, expect, it } from "vitest";
import {
  RESUME_GAP_MS, ResumeGapDetector, ResumeTransitionDetector, probeResumedLink,
} from "./useDesktopResumeRecovery";

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

  it("resets a background timer gap instead of treating it as a resume", () => {
    const detector = new ResumeGapDetector(1_000);
    detector.reset(1_000 + RESUME_GAP_MS * 4);
    expect(detector.observe(1_000 + RESUME_GAP_MS * 4 + 1)).toBe(false);
  });

  it("recovers only after an offline-to-online transition", () => {
    const detector = new ResumeTransitionDetector(false);
    expect(detector.network(false)).toBe(false);
    expect(detector.network(true)).toBe(true);
    expect(detector.network(true)).toBe(false);
  });
});

describe("resumed link probe", () => {
  const neverFires = () => undefined;

  it("keeps a link that answers", async () => {
    await expect(probeResumedLink(() => Promise.resolve(), 50, neverFires)).resolves.toBe("alive");
  });

  it("rebuilds a link that refuses", async () => {
    await expect(probeResumedLink(() => Promise.reject(new Error("no client")), 50, neverFires)).resolves.toBe("dead");
  });

  it("rebuilds a link that throws before sending", async () => {
    await expect(probeResumedLink(() => { throw new Error("gone"); }, 50, neverFires)).resolves.toBe("dead");
  });

  it("rebuilds a link that never answers, at the timeout", async () => {
    let fire: (() => void) | undefined;
    let armedFor: number | undefined;
    const outcome = probeResumedLink(() => new Promise(() => undefined), 3_000, (callback, ms) => {
      fire = callback;
      armedFor = ms;
    });
    expect(armedFor).toBe(3_000);
    fire?.();
    await expect(outcome).resolves.toBe("dead");
  });

  it("takes the first answer only", async () => {
    let fire: (() => void) | undefined;
    const outcome = probeResumedLink(() => Promise.resolve(), 50, (callback) => { fire = callback; });
    await expect(outcome).resolves.toBe("alive");
    fire?.();
    await expect(outcome).resolves.toBe("alive");
  });
});
