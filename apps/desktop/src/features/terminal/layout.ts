import type { CSSProperties } from "react";
import type { Pane } from "../../app/types";

export interface WindowGrid {
  width: number;
  height: number;
}

export function windowGrid(panes: Pane[]): WindowGrid {
  return panes.reduce(
    (grid, pane) => ({
      width: Math.max(grid.width, pane.left + pane.width),
      height: Math.max(grid.height, pane.top + pane.height),
    }),
    { width: 1, height: 1 },
  );
}

export function paneStyle(pane: Pane, grid: WindowGrid): CSSProperties {
  return {
    left: `${(pane.left / grid.width) * 100}%`,
    top: `${(pane.top / grid.height) * 100}%`,
    width: `${(pane.width / grid.width) * 100}%`,
    height: `${(pane.height / grid.height) * 100}%`,
  };
}

export function renderedPanes(panes: Pane[], zoomed: boolean): Pane[] {
  if (!zoomed) return panes;
  const active = panes.find((pane) => pane.active);
  return active ? [active] : panes.slice(0, 1);
}

export function renderedPaneStyle(pane: Pane, grid: WindowGrid, zoomed: boolean): CSSProperties {
  return zoomed
    ? { left: 0, top: 0, width: "100%", height: "100%" }
    : paneStyle(pane, grid);
}

export function resizeCellsFromPixels(deltaPixels: number, panePixels: number, paneCells: number): number {
  if (!Number.isFinite(deltaPixels) || !Number.isFinite(panePixels) || panePixels <= 0 || paneCells <= 0) return 1;
  const cellPixels = panePixels / paneCells;
  return Math.max(1, Math.round(Math.abs(deltaPixels) / cellPixels));
}

export type PaneDirection = "left" | "right" | "up" | "down";

export function adjacentPane(panes: Pane[], active: Pane, direction: PaneDirection): Pane | undefined {
  const activeCenterX = active.left + active.width / 2;
  const activeCenterY = active.top + active.height / 2;
  return panes
    .filter((pane) => pane.id !== active.id)
    .map((pane) => {
      const centerX = pane.left + pane.width / 2;
      const centerY = pane.top + pane.height / 2;
      const primary = direction === "left" ? activeCenterX - centerX
        : direction === "right" ? centerX - activeCenterX
          : direction === "up" ? activeCenterY - centerY
            : centerY - activeCenterY;
      const secondary = direction === "left" || direction === "right"
        ? Math.abs(centerY - activeCenterY)
        : Math.abs(centerX - activeCenterX);
      return { pane, primary, distance: primary * primary + secondary * secondary * 4 };
    })
    .filter((candidate) => candidate.primary > 0)
    .sort((a, b) => a.distance - b.distance)[0]?.pane;
}
