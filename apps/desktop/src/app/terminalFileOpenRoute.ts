import type { Pane } from "./types";

type PaneRoute = Pick<Pane, "id" | "sessionId" | "windowId" | "currentPath">;

/**
 * Returns the live pane only while the host resolution still describes the
 * same authoritative pane route that initiated it.
 */
export function currentTerminalFilePane(
  captured: PaneRoute,
  livePanes: readonly Pane[],
  currentTopologyGeneration: number,
  resolvedTopologyGeneration: string,
): Pane | undefined {
  if (resolvedTopologyGeneration !== String(currentTopologyGeneration)) return undefined;
  const live = livePanes.find((pane) => pane.id === captured.id);
  if (!live
    || live.sessionId !== captured.sessionId
    || live.windowId !== captured.windowId
    || live.currentPath !== captured.currentPath) return undefined;
  return live;
}
