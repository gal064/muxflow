import type { Pane } from "./types";

type PaneRoute = Pick<Pane, "id" | "sessionId" | "windowId" | "currentPath">;

/**
 * Returns the live pane only while the host resolution still describes the
 * same authoritative pane route that initiated it.
 *
 * The generation comparison is monotonic against the generation the request
 * carried, not an equality against the latest one: an unrelated topology event
 * on a busy pane advances the generation without moving this pane, and the
 * route comparison below is what decides whether the answer still applies.
 */
export function currentTerminalFilePane(
  captured: PaneRoute,
  livePanes: readonly Pane[],
  requestedTopologyGeneration: number,
  resolvedTopologyGeneration: string,
): Pane | undefined {
  if (Number(resolvedTopologyGeneration) < requestedTopologyGeneration) return undefined;
  const live = livePanes.find((pane) => pane.id === captured.id);
  if (!live
    || live.sessionId !== captured.sessionId
    || live.windowId !== captured.windowId
    || live.currentPath !== captured.currentPath) return undefined;
  return live;
}
