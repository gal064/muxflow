import { describe, expect, it } from "vitest";
import {
  DEFAULT_ROW_HEIGHT,
  OVERSCAN_ROWS,
  WINDOW_ROW_THRESHOLD,
  rowWindow,
  scrollOffsetForRow,
} from "./explorerWindow";

const VIEWPORT = { rowHeight: 20, viewportHeight: 400 };

describe("rowWindow", () => {
  it("mounts every row of an ordinary tree, exactly as before", () => {
    const window = rowWindow({ ...VIEWPORT, rowCount: WINDOW_ROW_THRESHOLD, scrollTop: 0, focusIndex: 0 });
    expect(window).toEqual({ start: 0, end: WINDOW_ROW_THRESHOLD, leadingHeight: 0, trailingHeight: 0, windowed: false });
  });

  it("mounts one contiguous band plus overscan for a large directory", () => {
    const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop: 0, focusIndex: 0 });
    expect(window.windowed).toBe(true);
    expect(window.start).toBe(0);
    expect(window.end).toBe(20 + OVERSCAN_ROWS * 2);
    expect(window.leadingHeight).toBe(0);
    // Everything not mounted is still reserved, so the scrollbar describes the
    // whole directory rather than the slice.
    expect(window.leadingHeight + (window.end - window.start) * 20 + window.trailingHeight)
      .toBe(4_096 * 20);
  });

  it("follows the scroll offset and keeps the reserved height exact", () => {
    const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop: 20_000, focusIndex: 1_000 });
    expect(window.start).toBe(1_000 - OVERSCAN_ROWS);
    expect(window.leadingHeight).toBe(window.start * 20);
    expect(window.leadingHeight + (window.end - window.start) * 20 + window.trailingHeight)
      .toBe(4_096 * 20);
  });

  it("keeps the focused row mounted however far it is from the scroll offset", () => {
    // Focus jumped to the end (End key, scroll-to-item) before the viewport
    // caught up. A window that dropped it would leave roving focus pointing at
    // an element that is not in the document.
    const far = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop: 0, focusIndex: 4_095 });
    expect(far.start).toBeLessThanOrEqual(4_095);
    expect(far.end).toBe(4_096);
    const back = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop: 80_000, focusIndex: 0 });
    expect(back.start).toBe(0);
    expect(back.end).toBeGreaterThan(0);
  });

  it("never runs off either end of the directory", () => {
    for (const scrollTop of [-1_000, 0, 81_920, 10_000_000]) {
      for (const focusIndex of [-5, 0, 4_095, 99_999]) {
        const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop, focusIndex });
        expect(window.start).toBeGreaterThanOrEqual(0);
        expect(window.end).toBeLessThanOrEqual(4_096);
        expect(window.start).toBeLessThan(window.end);
        expect(window.leadingHeight).toBeGreaterThanOrEqual(0);
        expect(window.trailingHeight).toBeGreaterThanOrEqual(0);
      }
    }
  });

  it("falls back to a usable band when layout has reported no geometry", () => {
    const window = rowWindow({ rowCount: 4_096, rowHeight: 0, viewportHeight: 0, scrollTop: 0, focusIndex: 0 });
    expect(window.start).toBe(0);
    expect(window.end).toBeGreaterThan(OVERSCAN_ROWS);
    expect(window.end).toBeLessThan(4_096);
    expect(window.trailingHeight).toBe((4_096 - window.end) * DEFAULT_ROW_HEIGHT);
  });
});

describe("scrollOffsetForRow", () => {
  it("leaves a row the user can already see exactly where it is", () => {
    expect(scrollOffsetForRow({ index: 5, rowHeight: 20, scrollTop: 0, viewportHeight: 400 })).toBeUndefined();
    expect(scrollOffsetForRow({ index: 19, rowHeight: 20, scrollTop: 0, viewportHeight: 400 })).toBeUndefined();
  });

  it("scrolls the minimum needed to reveal a row above or below the viewport", () => {
    expect(scrollOffsetForRow({ index: 20, rowHeight: 20, scrollTop: 0, viewportHeight: 400 })).toBe(20);
    expect(scrollOffsetForRow({ index: 3, rowHeight: 20, scrollTop: 200, viewportHeight: 400 })).toBe(60);
  });
});
