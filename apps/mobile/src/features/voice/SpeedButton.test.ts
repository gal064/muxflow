import { describe, expect, it } from "vitest";

import { formatRate, nextPlaybackRate } from "./speed";

describe("SpeedButton", () => {
  it("cycles exactly 1× → 1.5× → 2× → 1×", () => {
    expect(nextPlaybackRate(1)).toBe(1.5);
    expect(nextPlaybackRate(1.5)).toBe(2);
    expect(nextPlaybackRate(2)).toBe(1);
  });

  it("formats the visible rate", () => {
    expect(formatRate(1.5)).toBe("1.5×");
  });
});
