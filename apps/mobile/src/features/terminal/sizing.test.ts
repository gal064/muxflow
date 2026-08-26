import { describe, expect, it } from "vitest";
import { computeGrid, MIN_COLS, MIN_ROWS, NOMINAL_CELL, sameGrid } from "./sizing";

describe("sizing rule (§9.5, D6)", () => {
  it("floors the viewport to whole cells: ≈ 46 × 40 on a 390 dp phone with the keyboard hidden", () => {
    // 390 wide; 844 tall minus 48 header, 40 chips, 56 input bar, ~60 status/nav.
    expect(computeGrid({ width: 390, height: 640 })).toEqual({ cols: 50, rows: 41 });
    expect(computeGrid({ width: 360, height: 624 }, NOMINAL_CELL)).toEqual({ cols: 46, rows: 40 });
  });

  it("uses the measured cell, not the nominal one", () => {
    expect(computeGrid({ width: 400, height: 300 }, { width: 8, height: 16 })).toEqual({ cols: 50, rows: 18 });
  });

  it("never proposes less than tmux accepts, and survives an unmeasured viewport", () => {
    expect(computeGrid({ width: 0, height: 0 })).toEqual({ cols: MIN_COLS, rows: MIN_ROWS });
    expect(computeGrid({ width: Number.NaN, height: 10 })).toEqual({ cols: MIN_COLS, rows: MIN_ROWS });
    expect(computeGrid({ width: 100, height: 100 }, { width: 0, height: 0 })).toEqual({ cols: MIN_COLS, rows: MIN_ROWS });
  });

  it("sameGrid compares by value and treats undefined as different", () => {
    expect(sameGrid({ cols: 1, rows: 2 }, { cols: 1, rows: 2 })).toBe(true);
    expect(sameGrid({ cols: 1, rows: 2 }, { cols: 1, rows: 3 })).toBe(false);
    expect(sameGrid(undefined, { cols: 1, rows: 2 })).toBe(false);
  });
});
