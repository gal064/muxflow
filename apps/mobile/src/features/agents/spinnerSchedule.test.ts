import { describe, expect, it } from "vitest";

import { SPINNER_ARC_TURNS, SPINNER_TRACK_OPACITY, SPINNER_TURN_MS } from "./spinnerSchedule";

describe("spinner schedule (§9.3.1: the desktop's turn, on the desktop's track)", () => {
  it("turns once per 0.9 s, the desktop `.spinner`'s rate", () => {
    expect(SPINNER_TURN_MS).toBe(900);
  });

  it("draws an arc that reads as an arc in a still frame: longer than the desktop's quarter, on the desktop's track", () => {
    expect(SPINNER_ARC_TURNS).toBeGreaterThan(0.25);
    expect(SPINNER_ARC_TURNS).toBeLessThan(0.5);
    expect(SPINNER_TRACK_OPACITY).toBe(0.35);
  });
});
