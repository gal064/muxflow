import { describe, expect, it } from "vitest";
import type { Pane } from "../../app/types";
import { windowGrid } from "./layout";
import { cellsForBox, type PixelBox, type TerminalMeasurements, type TerminalSize } from "./TerminalRenderer";
import { clientSizeForSurface, MAX_CLIENT_CELLS, windowCellSize } from "./clientSize";

/** One terminal's measurements, fixed so every expectation below is exact. */
const CELL: PixelBox = { width: 8, height: 17 };
const CHROME = { horizontal: 12, vertical: 12, scrollbar: 14 };
const MEASUREMENTS: TerminalMeasurements = { cell: CELL, chrome: CHROME };

/** The size a surface of this many pixels is worth, independent of everything else. */
function surfaceSize(surface: PixelBox): TerminalSize {
  return cellsForBox(surface, CELL, CHROME)!;
}

/**
 * The computation this stage replaced (P12-U006): the active pane's measured
 * box scaled by its share of the topology. It is reproduced here — and nowhere
 * in the product — so each case below can show what the old formula would have
 * asked tmux for, which is the number that damaged the user's windows.
 */
function shareScaled(panes: Pane[], active: Pane, measured: TerminalSize): TerminalSize {
  const grid = windowGrid(panes);
  return {
    columns: Math.max(2, Math.round((measured.columns * grid.width) / active.width)),
    rows: Math.max(2, Math.round((measured.rows * grid.height) / active.height)),
  };
}

function pane(id: string, geometry: Partial<Pane>): Pane {
  return {
    id, sessionId: "$1", windowId: "@1", index: 0, active: false,
    left: 0, top: 0, width: 80, height: 24, currentCommand: "bash", currentPath: "/",
    ...geometry,
  };
}

describe("clientSizeForSurface", () => {
  // The three field cases from P12-U006. In every one the answer is the
  // surface's own size, and the geometry each case carries is what the replaced
  // formula would have multiplied by.
  const surface: PixelBox = { width: 1000, height: 800 };
  const measuredActiveBox: TerminalSize = { columns: 122, rows: 46 };

  it("ignores a zoomed pane, whose box is the window and whose topology height is one split", () => {
    const panes = [
      pane("%1", { active: true, width: 108, height: 9 }),
      pane("%2", { top: 10, width: 108, height: 40 }),
    ];
    const legacy = shareScaled(panes, panes[0], measuredActiveBox);
    expect(legacy.rows).toBeGreaterThan(250); // 108x251: the shape of the damage.
    expect(clientSizeForSurface(surface, MEASUREMENTS)).toEqual({ kind: "size", size: surfaceSize(surface) });
  });

  it("ignores a pane two columns wide", () => {
    const panes = [
      pane("%1", { active: true, width: 2, height: 24 }),
      pane("%2", { left: 3, width: 105, height: 24 }),
    ];
    expect(shareScaled(panes, panes[0], measuredActiveBox).columns).toBeGreaterThan(MAX_CLIENT_CELLS);
    expect(clientSizeForSurface(surface, MEASUREMENTS)).toEqual({ kind: "size", size: surfaceSize(surface) });
  });

  it("ignores a box and a topology snapshot that disagree mid-layout-change", () => {
    // Mid-split: the topology already reports both panes, so the window grid is
    // 50 rows, while the active pane still carries its pre-split height of 24
    // and its box has already been re-laid out. Nothing here is a steady state,
    // and the old formula sized tmux from it anyway.
    const panes = [
      pane("%1", { active: true, width: 108, height: 24 }),
      pane("%2", { top: 25, width: 108, height: 25 }),
    ];
    expect(shareScaled(panes, panes[0], { columns: 108, rows: 23 }).rows).not.toBe(surfaceSize(surface).rows);
    expect(clientSizeForSurface(surface, MEASUREMENTS)).toEqual({ kind: "size", size: surfaceSize(surface) });
  });

  it("is a fixed point: feeding the request back as the new topology cannot ratchet", () => {
    // Ten rounds of the loop behind the 298/310/314-row windows: tmux applies
    // the requested size, the layout hands the active pane a share of it, the
    // pane measures again, and the result is the next request. Every share from
    // the whole window down to an eighth is exercised. The legacy formula runs
    // on the same loop so the property is visibly about the change and not
    // about a loop that never moves.
    const first = clientSizeForSurface(surface, MEASUREMENTS);
    expect(first).toEqual({ kind: "size", size: surfaceSize(surface) });
    let legacyGrid = surfaceSize(surface);
    for (let round = 1; round <= 10; round += 1) {
      for (const share of [1, 2, 3, 4, 8]) {
        const height = Math.max(2, Math.round(legacyGrid.rows / share));
        const panes = [
          pane("%1", { active: true, width: legacyGrid.columns, height }),
          pane("%2", { top: height + 1, width: legacyGrid.columns, height: legacyGrid.rows - height - 1 }),
        ];
        legacyGrid = shareScaled(panes, panes[0], measuredActiveBox);
        // tmux applied whatever was asked for; ask again from the same surface.
        expect(clientSizeForSurface(surface, MEASUREMENTS)).toEqual(first);
      }
    }
    // The loop is not vacuous: the replaced formula walks a 46-row surface into
    // a window taller than any display, which is the field symptom.
    expect(legacyGrid.rows).toBeGreaterThan(250);
  });

  it("refuses a size above the bound, and stays quiet about a small window", () => {
    const tallCell: PixelBox = { width: 1, height: 1 };
    const refused = clientSizeForSurface({ width: 4000, height: 4000 }, { cell: tallCell, chrome: CHROME });
    expect(refused.kind).toBe("refused");
    expect(refused.kind === "refused" && refused.reason).toContain(`${MAX_CLIENT_CELLS} cell bound`);
    expect(refused.kind === "refused" && refused.reason).toContain("3974x3988");

    // Dragging the window narrow is not a defect and must not be reported as
    // one: below the minimum the answer is the same "nothing to ask for" as an
    // unmounted surface, and nothing retries it into a loop.
    expect(clientSizeForSurface({ width: 40, height: 800 }, MEASUREMENTS)).toEqual({ kind: "none" });
    expect(clientSizeForSurface({ width: 30, height: 20 }, MEASUREMENTS)).toEqual({ kind: "none" });
  });

  it("asks for nothing while the surface or the terminals cannot be measured", () => {
    expect(clientSizeForSurface(undefined, MEASUREMENTS)).toEqual({ kind: "none" });
    expect(clientSizeForSurface({ width: Number.NaN, height: 800 }, MEASUREMENTS)).toEqual({ kind: "none" });
    expect(clientSizeForSurface({ width: 0, height: 0 }, MEASUREMENTS)).toEqual({ kind: "none" });
    // No terminal has reported yet; the hook recomputes when one does.
    expect(clientSizeForSurface(surface, undefined)).toEqual({ kind: "none" });
  });
});

  it("gives every pane a box that can render the grid tmux derives from it", () => {
    // The request is a whole-surface number; tmux hands each pane a share of it
    // and the app renders that pane at tmux's grid, clipped by its frame. If a
    // pane's box is one pixel short of its grid, its last row is cut — the
    // symptom this stage exists to remove — so the property is checked over
    // every surface height in a realistic range and every split up to eight.
    for (let height = 400; height <= 1200; height += 1) {
      const decision = clientSizeForSurface({ width: 1000, height }, MEASUREMENTS);
      if (decision.kind !== "size") continue;
      const rows = decision.size.rows;
      for (const panes of [2, 3, 4, 8]) {
        // tmux spends one row per divider and splits the rest.
        const share = Math.floor((rows - (panes - 1)) / panes);
        if (share < 2) continue;
        // `paneStyle` gives the pane its share of the surface's pixels.
        const box = (share / rows) * height;
        expect(Math.floor(box / CELL.height)).toBeGreaterThanOrEqual(share);
      }
    }
  });

describe("cellsForBox", () => {
  it("subtracts the terminal's own padding and scrollbar before dividing", () => {
    // 1000 − 12 padding − 14 scrollbar = 974 → 121 columns of 8 px;
    // 800 − 12 padding = 788 → 46 rows of 17 px, the last partial row dropped.
    expect(cellsForBox({ width: 1000, height: 800 }, CELL, CHROME)).toEqual({ columns: 121, rows: 46 });
  });

  it("answers nothing when the box or the metrics cannot produce a cell", () => {
    expect(cellsForBox({ width: 20, height: 800 }, CELL, CHROME)).toBeUndefined();
    expect(cellsForBox({ width: 1000, height: 8 }, CELL, CHROME)).toBeUndefined();
    expect(cellsForBox({ width: 1000, height: 800 }, { width: 0, height: 17 }, CHROME)).toBeUndefined();
  });
});

describe("windowCellSize", () => {
  /**
   * tmux counts the separator row and column into the *following* pane's
   * offset, so the bottom-right pane's offset plus its own size lands exactly
   * on the window's edge — which is the only reason this can be read off pane
   * geometry at all.
   */
  it("reads the window's grid off its bottom-right pane", () => {
    // The bottom-right pane is listed *first* on purpose. Snapshot pane order
    // is not a guaranteed property, so a fixture that happened to end on the
    // widest and tallest pane would pass on an implementation that simply took
    // the last one — and a wrong answer here is a permanent `actualSize`
    // mismatch, which spends the reassertion budget resizing real windows on
    // every focus gain.
    const panes = [
      pane("%3", { left: 95, top: 25, width: 93, height: 25 }),
      pane("%1", { left: 0, top: 0, width: 94, height: 50 }),
      pane("%2", { left: 95, top: 0, width: 93, height: 24 }),
    ];
    expect(windowCellSize(panes, "@1")).toEqual({ columns: 188, rows: 50 });
    // Widest and tallest come from different panes, so neither axis can be
    // satisfied by picking one pane and reading both numbers off it.
    expect(windowCellSize([
      pane("%1", { left: 0, top: 0, width: 200, height: 10 }),
      pane("%2", { left: 0, top: 11, width: 20, height: 40 }),
    ], "@1")).toEqual({ columns: 200, rows: 51 });
  });

  it("ignores panes belonging to other windows", () => {
    const panes = [
      pane("%1", { windowId: "@1", width: 80, height: 24 }),
      pane("%2", { windowId: "@2", width: 200, height: 60 }),
    ];
    expect(windowCellSize(panes, "@1")).toEqual({ columns: 80, rows: 24 });
  });

  /**
   * "No answer", never a window of zero cells: the caller compares this against
   * a requested size, and a zero would read as every request being ignored.
   */
  it("answers nothing for a window it cannot describe", () => {
    expect(windowCellSize([pane("%1", {})], "@missing")).toBeUndefined();
    expect(windowCellSize([pane("%1", {})], undefined)).toBeUndefined();
    expect(windowCellSize([], "@1")).toBeUndefined();
  });
});
