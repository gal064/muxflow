import { invoke } from "@tauri-apps/api/core";
import type { AgentClient } from "./api";
import type {
  AgentFocus,
  AgentNativeNotification,
  AgentNotificationInstrumentation,
  AgentNotificationRoute,
  AgentRecord,
  AgentRequestScope,
} from "./types";
import { generationAtLeast, generationIsAfter, type AgentGeneration } from "./generation";

export interface NativeNotificationReceipt { id: number; actionable: boolean }

export function emitNativeAgentNotification(notification: AgentNativeNotification): Promise<NativeNotificationReceipt> {
  return invoke("emit_agent_notification", {
    notification: {
      title: notification.title,
      body: notification.body,
      requestAction: notification.requestAction,
      route: {
        hostProfile: notification.route.hostProfileId,
        serverIdentity: notification.route.serverIdentity,
        sessionId: notification.route.sessionId,
        sessionName: notification.route.sessionName,
        windowId: notification.route.windowId,
        windowName: notification.route.windowName,
        paneId: notification.route.paneId,
        agentId: notification.route.agentId,
        attentionGeneration: notification.route.attentionGeneration,
      },
    },
  });
}

/** A native click acknowledges only its immutable route identity, never pane peers or a newer generation. */
export function acknowledgeNotificationActivation(
  client: Pick<AgentClient, "markSeen">,
  scope: AgentRequestScope,
  route: Pick<AgentNotificationRoute, "agentId" | "attentionGeneration">,
): Promise<void> {
  return client.markSeen(scope, route.agentId, route.attentionGeneration);
}

export interface NotificationTransitionContext {
  focus: AgentFocus;
  replayed: boolean;
  previouslyNotifiedGeneration?: AgentGeneration;
  workspaceName?: string;
  windowName?: string;
  reconciledSnapshot?: boolean;
}

export type NotificationDecision =
  | { kind: "emit"; notification: AgentNativeNotification; instrumentation: AgentNotificationInstrumentation }
  | { kind: "ignore" }
  | { kind: "suppress"; instrumentation: AgentNotificationInstrumentation };

export function decideAgentNotification(
  previous: AgentRecord | undefined,
  next: AgentRecord,
  context: NotificationTransitionContext,
): NotificationDecision {
  const attentionAdvanced = Boolean(previous
    && generationIsAfter(next.attentionGeneration, previous.attentionGeneration));
  const persistedEvent = attentionAdvanced ? next.attentionKind : undefined;
  const event = persistedEvent
    ?? (next.lifecycle === "blocked" && (previous?.lifecycle !== "blocked"
      || Boolean(context.reconciledSnapshot && attentionAdvanced))
      ? "blocked" as const
      : previous?.lifecycle === "working" && next.lifecycle === "idle" && attentionAdvanced
        ? "completed" as const
        : undefined);
  if (!event) return { kind: "ignore" };
  const generation = next.attentionGeneration;
  const base = { agentId: next.id, event, generation };
  if (context.replayed) return { kind: "suppress", instrumentation: { ...base, outcome: "suppressed-replay" } };
  if (context.previouslyNotifiedGeneration !== undefined && generationAtLeast(context.previouslyNotifiedGeneration, generation)) {
    return { kind: "suppress", instrumentation: { ...base, outcome: "suppressed-duplicate" } };
  }
  const focusedPane = context.focus.appFocused && context.focus.terminalVisible
    && context.focus.hostProfileId === next.hostProfileId
    && context.focus.serverIdentity === next.serverIdentity
    && context.focus.sessionId === next.sessionId
    && context.focus.windowId === next.windowId
    && context.focus.paneId === next.paneId;
  if (focusedPane) return { kind: "suppress", instrumentation: { ...base, outcome: "suppressed-focused" } };
  const workspace = safeNotificationLabel(context.workspaceName ?? next.sessionName, "Workspace", 96);
  const terminal = safeNotificationLabel(context.windowName ?? next.windowName, "Terminal", 96);
  const agent = safeNotificationLabel(next.displayName, "Agent", 96);
  return {
    kind: "emit",
    instrumentation: { ...base, outcome: "emitted" },
    notification: {
      event,
      title: `${agent} ${event === "blocked" ? "needs attention" : "finished"}`,
      // Deliberately excludes prompt, output, paths, and hook payloads.
      body: !next.paneId
        ? `Unmapped · ${event === "blocked" ? "Blocked" : "Done"} · Review in Agents`
        : `${workspace} · ${terminal} · ${event === "blocked" ? "Blocked" : "Done"}`,
      requestAction: Boolean(next.paneId),
      route: {
        hostProfileId: next.hostProfileId,
        serverIdentity: next.serverIdentity,
        sessionId: next.sessionId,
        sessionName: next.sessionName,
        windowId: next.windowId,
        windowName: next.windowName,
        paneId: next.paneId,
        agentId: next.id,
        attentionGeneration: generation,
      },
    },
  };
}

function safeNotificationLabel(value: string, fallback: string, maxBytes: number): string {
  const normalized = value.replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim() || fallback;
  const encoder = new TextEncoder();
  let result = "";
  for (const character of normalized) {
    if (encoder.encode(result + character).byteLength > maxBytes) break;
    result += character;
  }
  return result || fallback;
}
