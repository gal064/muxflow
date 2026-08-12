import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type { HostProfile, Pane, TmuxSnapshot } from "../../app/types";
import { notificationTopology, paneForResolvedNotification, type NotificationPaneRoute, type ResolvedNotificationRoute } from "../../app/paneRouting";
import type { AgentClient } from "./api";
import { acknowledgeNotificationActivation } from "./notifications";
import type { AgentRequestScope } from "./types";

export interface PaneSurfaceResult {
  ok: boolean;
  error?: unknown;
}

interface ActivationOptions {
  agentClient: AgentClient;
  agentScope?: AgentRequestScope;
  currentHostProfileId: string;
  connected: boolean;
  connectionEpoch: number;
  focusedPaneId?: string;
  profiles: readonly HostProfile[];
  snapshot: TmuxSnapshot;
  setStatus(message: string): void;
  requestReconnect(): void;
  switchHostProfile(profile: HostProfile): void;
  surfacePaneDestination(target: Pane, source: string, successMessage?: string): Promise<PaneSurfaceResult>;
}

interface PendingActivation {
  payload: NotificationPaneRoute;
  retryUsed: boolean;
  previousClientId?: string;
  previousEpoch: number;
}

const FOCUS_GUARD_TIMEOUT_MS = 15_000;

interface FocusGuard {
  token: number;
  paneId: string;
  timer: ReturnType<typeof setTimeout>;
}

/** Owns native-route activation so App only supplies current authoritative shell state. */
export function useAgentNotificationActivation(options: ActivationOptions) {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const pending = useRef<PendingActivation | undefined>(undefined);
  const focusGuard = useRef<FocusGuard | undefined>(undefined);
  const nextFocusGuardToken = useRef(0);
  const [focusGuardPaneId, setFocusGuardPaneId] = useState<string>();

  const clearFocusGuard = useCallback((token?: number) => {
    const current = focusGuard.current;
    if (!current || (token !== undefined && current.token !== token)) return;
    clearTimeout(current.timer);
    focusGuard.current = undefined;
    setFocusGuardPaneId(undefined);
  }, []);

  const beginFocusGuard = useCallback((paneId: string): number => {
    clearFocusGuard();
    const token = ++nextFocusGuardToken.current;
    const timer = setTimeout(() => clearFocusGuard(token), FOCUS_GUARD_TIMEOUT_MS);
    focusGuard.current = { token, paneId, timer };
    setFocusGuardPaneId(paneId);
    return token;
  }, [clearFocusGuard]);

  useEffect(() => () => {
    if (focusGuard.current) clearTimeout(focusGuard.current.timer);
    focusGuard.current = undefined;
  }, []);

  const queueForFreshConnection = useCallback((payload: NotificationPaneRoute, retryUsed: boolean) => {
    const current = optionsRef.current;
    pending.current = {
      payload,
      retryUsed,
      previousClientId: current.agentScope?.clientId,
      previousEpoch: current.connectionEpoch,
    };
  }, []);

  const activate = useCallback(async (payload: NotificationPaneRoute, retryUsed = false): Promise<boolean> => {
    const current = optionsRef.current;
    if (!payload.paneId) {
      current.setStatus("This agent notification is unmapped and remains available in Agents; no terminal destination was focused.");
      return false;
    }
    if (payload.hostProfile !== current.currentHostProfileId) {
      const targetProfile = current.profiles.find((profile) => profile.id === payload.hostProfile);
      if (!targetProfile) {
        current.setStatus("Notification host profile no longer exists; the in-app attention item remains available on its configured host.");
        return false;
      }
      queueForFreshConnection(payload, retryUsed);
      current.switchHostProfile(targetProfile);
      current.setStatus(`Connecting to ${targetProfile.label} before resolving the notification…`);
      return false;
    }
    if (!current.connected || !current.agentScope) {
      queueForFreshConnection(payload, retryUsed);
      current.setStatus("Reconnecting before resolving the notification against authoritative topology…");
      current.requestReconnect();
      return false;
    }

    const capturedScope = current.agentScope;
    try {
      const resolved = await invoke<ResolvedNotificationRoute>("resolve_notification_route", {
        route: payload,
        topology: notificationTopology(current.snapshot, capturedScope.serverIdentity),
      });
      if (!sameAgentConnection(capturedScope, optionsRef.current.agentScope)) return false;
      const latest = optionsRef.current;
      const destination = paneForResolvedNotification(latest.snapshot, resolved);
      if (destination.kind === "unavailable") {
        latest.setStatus(`Notification destination unavailable: ${destination.reason}.`);
        return false;
      }
      const guardToken = beginFocusGuard(destination.pane.id);
      try {
        const surfaced = await latest.surfacePaneDestination(
          destination.pane,
          "Notification",
        );
        if (!surfaced.ok) {
          if (!retryUsed && isStaleFocusError(surfaced.error)) {
            queueForFreshConnection(payload, true);
            latest.setStatus("Notification topology changed while focusing; refreshing once before resolving the exact destination…");
            latest.requestReconnect();
            return false;
          }
          latest.setStatus(`Notification destination could not be focused${surfaced.error ? `: ${String(surfaced.error)}` : "."}`);
          return false;
        }
        const exactScope = optionsRef.current.agentScope;
        if (!exactScope || !sameAgentConnection(capturedScope, exactScope)) return false;
        try {
          await acknowledgeNotificationActivation(latest.agentClient, exactScope, payload);
        } catch (error) {
          latest.setStatus(`Notification opened, but its exact attention generation was not acknowledged: ${String(error)}`);
          return false;
        }
        return true;
      } finally {
        clearFocusGuard(guardToken);
      }
    } catch (error) {
      current.setStatus(`Could not resolve notification: ${String(error)}`);
      return false;
    }
  }, [beginFocusGuard, clearFocusGuard, queueForFreshConnection]);

  const activationHandler = useRef<(route: NotificationPaneRoute) => void>(() => undefined);
  activationHandler.current = (payload) => { void activate(payload); };
  useEffect(() => {
    const unlisten = listen<NotificationPaneRoute>("notification-activated", ({ payload }) => activationHandler.current(payload));
    return () => void unlisten.then((dispose) => dispose());
  }, []);

  useEffect(() => {
    const waiting = pending.current;
    if (!waiting || !options.connected || !options.agentScope
      || options.currentHostProfileId !== waiting.payload.hostProfile) return;
    const freshClient = waiting.previousClientId === undefined || options.agentScope.clientId !== waiting.previousClientId;
    const freshEpoch = options.connectionEpoch > waiting.previousEpoch;
    if (!freshClient && !freshEpoch) return;
    pending.current = undefined;
    void activate(waiting.payload, waiting.retryUsed);
  }, [activate, options.agentScope?.clientId, options.connected, options.connectionEpoch, options.currentHostProfileId]);

  return {
    activateNotificationRoute: activate,
    automaticSeen: focusGuardPaneId !== options.focusedPaneId,
    clearNotificationFocusGuard: clearFocusGuard,
  };
}

function sameAgentConnection(left: AgentRequestScope, right: AgentRequestScope | undefined): boolean {
  return Boolean(right
    && left.clientId === right.clientId
    && left.connectionEpoch === right.connectionEpoch
    && left.hostProfileId === right.hostProfileId
    && left.serverIdentity === right.serverIdentity);
}

export function isStaleFocusError(error: unknown): boolean {
  return /stale|generation|no longer|not found|unknown (?:pane|window|session)|authoritative/i.test(String(error ?? ""));
}
