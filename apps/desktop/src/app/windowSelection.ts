import type { Window as TmuxWindow } from "./types";
import type { TmuxAction } from "../features/tmux/actions";

export type WindowMoveDirection = "left" | "right";

export function relativeWindowReorderAction(
  windows: readonly TmuxWindow[],
  activeWindowId: string,
  direction: WindowMoveDirection,
): TmuxAction | undefined {
  const ordered = [...windows].sort((left, right) => left.index - right.index);
  const current = ordered.findIndex((window) => window.id === activeWindowId);
  const target = ordered[current + (direction === "left" ? -1 : 1)];
  const active = ordered[current];
  if (!active || !target || target.sessionId !== active.sessionId || target.id === active.id) return undefined;
  return {
    kind: "reorderWindow",
    sessionId: active.sessionId,
    windowId: active.id,
    targetWindowId: target.id,
    relativePosition: direction === "left" ? "before" : "after",
  };
}

export function resolveActiveWindowId(
  windows: readonly TmuxWindow[],
  current: string | undefined,
): string | undefined {
  return windows.find((window) => window.active)?.id
    ?? windows.find((window) => window.id === current)?.id
    ?? windows[0]?.id;
}
