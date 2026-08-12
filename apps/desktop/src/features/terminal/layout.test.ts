import { describe, expect, it } from "vitest";
import type { Pane } from "../../app/types";
import { adjacentPane, paneStyle, renderedPaneStyle, renderedPanes, resizeCellsFromPixels, windowGrid } from "./layout";

const pane = (overrides: Partial<Pane>): Pane => ({
  id: "%1",
  sessionId: "$1",
  windowId: "@1",
  index: 0,
  active: true,
  width: 80,
  height: 24,
  left: 0,
  top: 0,
  currentPath: "/tmp",
  currentCommand: "shell",
  ...overrides,
});

describe("tmux pane geometry", () => {
  it("maps authoritative cell rectangles to split percentages", () => {
    const panes = [pane({ width: 80 }), pane({ id: "%2", left: 80, width: 80 })];
    const grid = windowGrid(panes);
    expect(grid).toEqual({ width: 160, height: 24 });
    expect(paneStyle(panes[1], grid)).toMatchObject({ left: "50%", width: "50%" });
  });

  it("finds the closest pane in a requested visual direction", () => {
    const active = pane({ width: 40, height: 12 });
    const right = pane({ id: "%2", left: 40, width: 40, height: 12, active: false });
    const down = pane({ id: "%3", top: 12, width: 40, height: 12, active: false });
    expect(adjacentPane([active, right, down], active, "right")?.id).toBe("%2");
    expect(adjacentPane([active, right, down], active, "down")?.id).toBe("%3");
    expect(adjacentPane([active, right, down], active, "left")).toBeUndefined();
  });

  it("mounts only the active renderer while authoritative zoom is enabled", () => {
    const active = pane({ id: "%1", active: true, width: 40 });
    const hidden = pane({ id: "%2", active: false, left: 40, width: 40 });
    expect(renderedPanes([active, hidden], true).map((item) => item.id)).toEqual(["%1"]);
    expect(renderedPaneStyle(active, windowGrid([active, hidden]), true)).toEqual({
      left: 0, top: 0, width: "100%", height: "100%",
    });
    expect(renderedPanes([active, hidden], false)).toHaveLength(2);
  });

  it("changes mounted pane IDs on zoom so component visibility leases hide and reveal the exact panes", () => {
    const active = pane({ id: "%1", active: true, width: 40 });
    const sibling = pane({ id: "%2", active: false, left: 40, width: 40 });
    const unzoomed = renderedPanes([active, sibling], false).map((item) => item.id);
    const zoomed = renderedPanes([active, sibling], true).map((item) => item.id);
    const transition = (from: string[], to: string[]) => ({
      hidden: from.filter((paneId) => !to.includes(paneId)),
      revealed: to.filter((paneId) => !from.includes(paneId)),
    });
    expect(transition(unzoomed, zoomed)).toEqual({ hidden: ["%2"], revealed: [] });
    expect(transition(zoomed, unzoomed)).toEqual({ hidden: [], revealed: ["%2"] });
    expect(zoomed).toEqual(["%1"]);
  });

  it("converts divider pixels through actual pane cell metrics on each axis", () => {
    expect(resizeCellsFromPixels(16, 160, 80)).toBe(8);
    expect(resizeCellsFromPixels(40, 480, 24)).toBe(2);
    expect(resizeCellsFromPixels(10, 0, 24)).toBe(1);
  });
});
