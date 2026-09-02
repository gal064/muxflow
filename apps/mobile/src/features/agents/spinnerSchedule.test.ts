import { describe, expect, it } from "vitest";

import {
  SPINNER_ARC_TURNS,
  SPINNER_REPAINTS_PER_SECOND,
  SPINNER_STEP_DEGREES,
  SPINNER_STEP_MS,
  SPINNER_STEPS,
  SPINNER_TRACK_OPACITY,
  SPINNER_TURN_MS,
} from "./spinnerSchedule";

describe("spinner schedule (§9.3.1: a handful of repaints a second, not sixty)", () => {
  it("steps tile one turn exactly, so the loop has no seam", () => {
    expect(SPINNER_STEP_DEGREES * SPINNER_STEPS).toBe(360);
    expect(Number.isInteger(SPINNER_STEP_DEGREES)).toBe(true);
    expect(SPINNER_STEPS * SPINNER_STEP_MS).toBe(SPINNER_TURN_MS);
  });

  it("repaints at most a dozen times a second and turns at a readable rate", () => {
    expect(SPINNER_REPAINTS_PER_SECOND).toBeLessThanOrEqual(12);
    expect(SPINNER_REPAINTS_PER_SECOND).toBeGreaterThanOrEqual(6);
    expect(SPINNER_TURN_MS).toBeGreaterThanOrEqual(800);
    expect(SPINNER_TURN_MS).toBeLessThanOrEqual(1500);
  });

  it("draws an arc that reads as an arc in a still frame: longer than the desktop's quarter, on a fainter track", () => {
    expect(SPINNER_ARC_TURNS).toBeGreaterThan(0.25);
    expect(SPINNER_ARC_TURNS).toBeLessThan(0.5);
    expect(SPINNER_TRACK_OPACITY).toBeLessThanOrEqual(0.35);
  });
});
