import { describe, expect, it } from "vitest";

import { BIG_PANE_FRACTION, bigPaneHeight, MIN_LIST_HEIGHT } from "./paneLayout";

describe("bigPaneHeight", () => {
  it("takes 70% of the height under the header on a tall phone", () => {
    // 800 dp window, 24 + 48 insets, 48 header → 680 available.
    expect(bigPaneHeight(800, 24, 48, 48)).toBe(Math.round(680 * BIG_PANE_FRACTION));
  });
  it("keeps the list at least MIN_LIST_HEIGHT on a short window", () => {
    // 480 dp available: 70% would leave 144 dp, under the floor.
    expect(bigPaneHeight(560, 24, 8, 48)).toBe(480 - MIN_LIST_HEIGHT);
  });
  it("never goes negative", () => {
    expect(bigPaneHeight(100, 24, 8, 48)).toBe(0);
  });
});
