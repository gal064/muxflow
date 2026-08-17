import { describe, expect, it } from "vitest";
import {
  DEFAULT_ROW_HEIGHT,
  OVERSCAN_ROWS,
  WINDOW_ROW_THRESHOLD,
  mountedRowCount,
  rowWindow,
  scrollOffsetForRow,
  type RowWindow,
} from "./explorerWindow";

const VIEWPORT = { rowHeight: 20, viewportHeight: 400 };
const ROW_HEIGHT = 20;

/** Every mounted index, in order. */
function mounted(window: RowWindow): number[] {
  return window.segments.flatMap((segment) =>
    Array.from({ length: segment.end - segment.start }, (_, offset) => segment.start + offset));
}

/**
 * Total height the tree occupies, mounted rows plus reserved gaps.
 *
 * The scrollbar has to describe the whole directory whatever is mounted, so
 * this must equal `rowCount * rowHeight` for every window.
 */
function contentHeight(window: RowWindow, rowHeight: number): number {
  const rows = window.segments.reduce(
    (total, segment) => total + (segment.end - segment.start) * rowHeight, 0);
  const gaps = window.segments.reduce((total, segment) => total + segment.leadingHeight, 0);
  return rows + gaps + window.trailingHeight;
}

/** The row indices the viewport is actually over at this scroll offset. */
function visibleRows(scrollTop: number, rowHeight: number, viewportHeight: number, rowCount: number): number[] {
  const first = Math.max(0, Math.floor(scrollTop / rowHeight));
  const last = Math.min(rowCount - 1, Math.ceil((scrollTop + viewportHeight) / rowHeight) - 1);
  return Array.from({ length: Math.max(0, last - first + 1) }, (_, offset) => first + offset);
}

describe("rowWindow", () => {
  it("mounts every row of an ordinary tree, exactly as before", () => {
    const window = rowWindow({ ...VIEWPORT, rowCount: WINDOW_ROW_THRESHOLD, scrollTop: 0, focusIndex: 0 });
    expect(window.windowed).toBe(false);
    expect(window.segments).toEqual([{ start: 0, end: WINDOW_ROW_THRESHOLD, leadingHeight: 0 }]);
    expect(window.trailingHeight).toBe(0);
    expect(rowWindow({ ...VIEWPORT, rowCount: 0, scrollTop: 0, focusIndex: 0 }).segments).toEqual([]);
  });

  it("mounts what the viewport is over, plus overscan", () => {
    const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop: 0, focusIndex: 0 });
    expect(window.windowed).toBe(true);
    expect(mountedRowCount(window)).toBe(20 + OVERSCAN_ROWS * 2);
    expect(contentHeight(window, ROW_HEIGHT)).toBe(4_096 * ROW_HEIGHT);
  });

  /**
   * The defect this shape exists to prevent: focus does not follow the
   * scrollbar, so a window that moved to the focused row instead of keeping the
   * scroll band left a wheel-scrolled directory rendering nothing at all.
   */
  it("always mounts the rows the viewport is over, however far focus has been left behind", () => {
    for (const scrollTop of [0, 2_000, 20_000, 81_900]) {
      const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop, focusIndex: 0 });
      const shown = new Set(mounted(window));
      for (const row of visibleRows(scrollTop, ROW_HEIGHT, VIEWPORT.viewportHeight, 4_096)) {
        expect(shown.has(row), `row ${row} was visible at ${scrollTop} and not mounted`).toBe(true);
      }
      expect(contentHeight(window, ROW_HEIGHT)).toBe(4_096 * ROW_HEIGHT);
    }
  });

  it("keeps the focused row mounted however far it is from the scroll offset", () => {
    // Focus jumped to the end (End key, scroll-to-item) before the viewport
    // caught up. A window that dropped it would leave roving focus pointing at
    // an element that is not in the document.
    for (const [scrollTop, focusIndex] of [[0, 4_095], [80_000, 0], [40_000, 7], [40_000, 4_000]]) {
      const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop, focusIndex });
      expect(mounted(window)).toContain(focusIndex);
      expect(contentHeight(window, ROW_HEIGHT)).toBe(4_096 * ROW_HEIGHT);
    }
  });

  it("emits ordered, non-overlapping segments whose reserved height is exact", () => {
    for (const scrollTop of [-1_000, 0, 3_333, 81_920, 10_000_000]) {
      for (const focusIndex of [-5, 0, 1_000, 4_095, 99_999]) {
        const window = rowWindow({ ...VIEWPORT, rowCount: 4_096, scrollTop, focusIndex });
        const indices = mounted(window);
        expect(indices).toEqual([...indices].sort((left, right) => left - right));
        expect(new Set(indices).size, "a row was mounted twice").toBe(indices.length);
        expect(indices[0]).toBeGreaterThanOrEqual(0);
        expect(indices.at(-1)).toBeLessThan(4_096);
        for (const segment of window.segments) expect(segment.leadingHeight).toBeGreaterThanOrEqual(0);
        expect(window.trailingHeight).toBeGreaterThanOrEqual(0);
        expect(contentHeight(window, ROW_HEIGHT)).toBe(4_096 * ROW_HEIGHT);
      }
    }
  });

  it("falls back to a usable band when layout has reported no geometry", () => {
    const window = rowWindow({ rowCount: 4_096, rowHeight: 0, viewportHeight: 0, scrollTop: 0, focusIndex: 0 });
    expect(mountedRowCount(window)).toBeGreaterThan(OVERSCAN_ROWS);
    expect(mountedRowCount(window)).toBeLessThan(4_096);
    expect(contentHeight(window, DEFAULT_ROW_HEIGHT)).toBe(4_096 * DEFAULT_ROW_HEIGHT);
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
