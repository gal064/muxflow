// The notification decision rule (design doc §13), as a pure function. Posting
// is the notifications feature's job (M4); this file has no expo imports.
// Simplified from apps/desktop/src/features/agents/notifications.ts::decideAgentNotification.

import type { Agent } from "../../store/sessionStore";

export type NotificationEvent = "blocked" | "completed";

export interface NotificationContext {
  /** `SessionState.notificationWatermark`. */
  notificationWatermark: bigint;
  /** Step 5: whether `(agentId, attentionGeneration)` was already posted in this process. */
  alreadyNotified: (agentId: string, attentionGeneration: bigint) => boolean;
  /** Step 6. */
  focusedPaneId: string | undefined;
  /** Agent shown by a focused agent-specific screen, such as Voice. */
  viewedAgentId: string | undefined;
  appInForeground: boolean;
  /** `agentWorkspaceName(state, next)` — the caller resolves it from the store. */
  workspaceName: string;
  /** The canonical task/tab name shown beside this agent's status mark. */
  agentName: string;
}

export interface NotificationToPost {
  kind: "post";
  event: NotificationEvent;
  title: string;
  body: string;
  /** Tag: a newer notification replaces the older one for the same agent. */
  tag: string;
  data: { agentId: string; paneId: string; sessionId: string; attentionGeneration: bigint };
}

export type NotificationDecision =
  | NotificationToPost
  | { kind: "skip"; reason: "noEvent" | "belowWatermark" | "alreadyNotified" | "focused" };

export function decideAgentNotification(
  prev: Agent | undefined,
  next: Agent,
  context: NotificationContext,
): NotificationDecision {
  // 2.
  const attentionAdvanced = prev !== undefined && next.attentionGeneration > prev.attentionGeneration;
  // 3.
  const event: string | undefined = attentionAdvanced
    ? next.attentionKind
    : next.lifecycle === "blocked" && prev?.lifecycle !== "blocked"
      ? "blocked"
      : undefined;
  if (event !== "blocked" && event !== "completed") return { kind: "skip", reason: "noEvent" };
  // 4.
  if (next.attentionGeneration <= context.notificationWatermark) return { kind: "skip", reason: "belowWatermark" };
  // 5.
  if (context.alreadyNotified(next.id, next.attentionGeneration)) return { kind: "skip", reason: "alreadyNotified" };
  // 6.
  const viewingAgent = context.viewedAgentId === next.id;
  const viewingTerminal = context.focusedPaneId !== undefined && context.focusedPaneId === next.route.paneId;
  if (context.appInForeground && (viewingAgent || viewingTerminal)) {
    return { kind: "skip", reason: "focused" };
  }
  // 7.
  return {
    kind: "post",
    event,
    title: `${context.workspaceName} · ${context.agentName}`,
    body: event === "blocked" ? "Needs your input" : "Finished",
    tag: next.id,
    data: {
      agentId: next.id,
      paneId: next.route.paneId,
      sessionId: next.route.sessionId,
      attentionGeneration: next.attentionGeneration,
    },
  };
}
