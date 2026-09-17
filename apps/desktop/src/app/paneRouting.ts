import type { Pane, TmuxSnapshot } from "./types";
import type { AgentGeneration } from "../features/agents/generation";

export interface NotificationPaneRoute {
  hostProfile: string;
  serverIdentity: string;
  sessionName: string;
  sessionId: string;
  windowName: string;
  windowId: string;
  paneId: string;
  agentId: string;
  attentionGeneration: AgentGeneration;
}

export type ResolvedNotificationRoute =
  | { resolution: "exact"; sessionId: string; windowId: string; paneId: string; attentionGeneration: AgentGeneration }
  | { resolution: "expired" }
  | { resolution: "wrongServer" };

export interface SyntheticNotificationTopology {
  serverIdentity: string;
  targets: Array<{ sessionId: string; sessionName: string; windowId: string; windowName: string; paneIds: string[] }>;
}

export type PaneDestination =
  | { kind: "target"; pane: Pane }
  | { kind: "unavailable"; reason: string };

/** Accepts only panes in the current authoritative tmux topology. */
export function resolveTerminalDestination(
  panes: readonly Pane[],
  paneId: string,
): PaneDestination {
  const source = panes.find((pane) => pane.id === paneId);
  if (!source) return { kind: "unavailable", reason: `pane ${paneId} is no longer available` };

  return { kind: "target", pane: source };
}

export function notificationTopology(snapshot: TmuxSnapshot, serverIdentity: string): SyntheticNotificationTopology {
  return {
    serverIdentity,
    targets: snapshot.windows.map((window) => ({
      sessionId: window.sessionId,
      sessionName: snapshot.sessions.find((session) => session.id === window.sessionId)?.name ?? window.sessionId,
      windowId: window.id,
      windowName: window.name,
      paneIds: snapshot.panes.filter((pane) => pane.windowId === window.id).map((pane) => pane.id),
    })),
  };
}

export function paneForResolvedNotification(snapshot: TmuxSnapshot, route: ResolvedNotificationRoute): PaneDestination {
  if (route.resolution === "expired" || route.resolution === "wrongServer") {
    return { kind: "unavailable", reason: route.resolution === "expired" ? "notification destination expired" : "notification belongs to a replaced tmux server" };
  }
  const pane = snapshot.panes.find((item) => item.id === route.paneId && item.windowId === route.windowId && item.sessionId === route.sessionId);
  return pane ? { kind: "target", pane } : { kind: "unavailable", reason: "exact notification destination is no longer available" };
}
