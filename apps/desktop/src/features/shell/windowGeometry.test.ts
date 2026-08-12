import { describe, expect, it } from "vitest";
import { captureWindowGeometry, restoredPhysicalSize, restoredWindowGeometry } from "./windowGeometry";

describe("scale-aware window geometry", () => {
  it("preserves logical size when moving from 100% to 200% device scale", () => {
    const geometry = captureWindowGeometry({ x: 20, y: 30 }, { width: 1_200, height: 800 }, false, 1);
    expect(geometry.scaleFactorMilli).toBe(1_000);
    expect(restoredPhysicalSize(geometry, 2)).toEqual({ width: 2_400, height: 1_600 });
  });

  it("scales down from 200% and preserves legacy physical geometry", () => {
    const geometry = captureWindowGeometry({ x: 0, y: 0 }, { width: 2_400, height: 1_600 }, true, 2);
    expect(restoredPhysicalSize(geometry, 1)).toEqual({ width: 1_200, height: 800 });
    expect(restoredPhysicalSize({ x: 0, y: 0, width: 1_200, height: 800, maximized: false }, 2))
      .toEqual({ width: 1_200, height: 800 });
  });

  it("bounds malformed runtime scale values to a safe default", () => {
    const geometry = captureWindowGeometry({ x: 0, y: 0 }, { width: 800, height: 500 }, false, Number.NaN);
    expect(geometry.scaleFactorMilli).toBe(1_000);
    expect(restoredPhysicalSize(geometry, Number.POSITIVE_INFINITY)).toEqual({ width: 800, height: 500 });
  });

  it("centers an off-screen window on the primary monitor after disconnect", () => {
    const primary = { position: { x: 0, y: 0 }, size: { width: 1_920, height: 1_080 }, scaleFactor: 1 };
    expect(restoredWindowGeometry(
      { x: 2_200, y: 200, width: 1_200, height: 800, maximized: false, scaleFactorMilli: 1_000 },
      [primary], primary, 1,
    )).toEqual({ x: 360, y: 140, width: 1_200, height: 800 });
  });

  it("retains a visible negative-origin monitor placement and rescales", () => {
    const left = { position: { x: -2_560, y: 0 }, size: { width: 2_560, height: 1_440 }, scaleFactor: 2 };
    const primary = { position: { x: 0, y: 0 }, size: { width: 1_920, height: 1_080 }, scaleFactor: 1 };
    expect(restoredWindowGeometry(
      { x: -2_500, y: 100, width: 1_200, height: 800, maximized: false, scaleFactorMilli: 1_000 },
      [left, primary], primary, 1,
    )).toEqual({ x: -2_500, y: 100, width: 2_400, height: 1_600 });
  });
});
