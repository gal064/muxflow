import { useMemo, type Dispatch, type SetStateAction } from "react";
import { useAgentWorkflow } from "../features/agents/AgentHookWorkflow";
import type { AgentClient } from "../features/agents/api";
import { agentHostIdentity, type AgentSoundPreferences } from "../features/agents/types";
import { useAgentHostSetup } from "../features/agents/useAgentHostSetup";
import { useAgentNotificationActivation } from "../features/agents/useAgentNotificationActivation";
import { useAgentRuntime } from "../features/agents/useAgentRuntime";
import type { ActiveRoot } from "../features/files/types";
import type { HostSetupDecision } from "../features/shell/types";
import type { HostProfile, Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "./types";
import type { PaneSurfaceResult } from "./useShellNavigation";

interface AppAgentControllerOptions {
  activePane?: Pane;
  activeRoot?: ActiveRoot;
  activeSession?: Session;
  activeSessionId?: string;
  activeWindow?: TmuxWindow;
  activeWindowId?: string;
  agentClient: AgentClient;
  /**
   * Consent for this host's agent setup that was already given elsewhere —
   * the remote helper install dialog, which names it. Passed straight through;
   * `useAgentHostSetup` is what decides whether it applies.
   */
  agentAutoSetup?: { hostProfileId: string; consume(): void };
  appFocused: boolean;
  clientHostProfileId?: string;
  clientId?: string;
  currentHostProfileId: string;
  decision?: HostSetupDecision;
  decisionsArePersistable: boolean;
  hostCanMutate: boolean;
  hostLabel: string;
  profiles: readonly HostProfile[];
  recordDecision(hostProfileId: string, decision: HostSetupDecision): void;
  requestReconnect(): void;
  selectedAppTab: boolean;
  serverIdentity?: string;
  setAgentModalOpen: Dispatch<SetStateAction<boolean>>;
  setStatus(message: string): void;
  snapshot: TmuxSnapshot;
  soundPreferences: AgentSoundPreferences;
  surfacePaneDestination(
    pane: Pane,
    source: string,
    successMessage?: string,
  ): Promise<PaneSurfaceResult>;
  switchHostProfile(profile: HostProfile): void;
  terminalEpoch: number;
  topologyGeneration: number;
}

/** Owns the agent notification/runtime/setup domain outside the shell component. */
export function useAppAgentController(options: AppAgentControllerOptions) {
  // The client profile follows the requested profile before the native client
  // does. During that handoff there is deliberately no writable agent scope.
  const scope = useMemo(() => options.clientId
    && options.clientHostProfileId === options.currentHostProfileId
    && options.serverIdentity && options.hostCanMutate ? {
    clientId: options.clientId,
    hostProfileId: options.currentHostProfileId,
    serverIdentity: options.serverIdentity,
    topologyGeneration: options.topologyGeneration,
    connectionEpoch: options.terminalEpoch,
  } : undefined, [
    options.clientHostProfileId, options.clientId, options.currentHostProfileId,
    options.hostCanMutate, options.serverIdentity, options.terminalEpoch, options.topologyGeneration,
  ]);
  const notificationActivation = useAgentNotificationActivation({
    agentClient: options.agentClient,
    agentScope: scope,
    connected: options.hostCanMutate && Boolean(options.serverIdentity),
    connectionEpoch: options.terminalEpoch,
    currentHostProfileId: options.currentHostProfileId,
    focusedPaneId: options.activePane?.id,
    profiles: options.profiles,
    snapshot: options.snapshot,
    requestReconnect: options.requestReconnect,
    setStatus: options.setStatus,
    surfacePaneDestination: options.surfacePaneDestination,
    switchHostProfile: options.switchHostProfile,
  });
  const focus = useMemo(() => ({
    hostProfileId: options.currentHostProfileId,
    serverIdentity: options.serverIdentity,
    sessionId: options.activeSessionId,
    windowId: options.activeWindowId,
    paneId: options.activePane?.id,
    appFocused: options.appFocused,
    terminalVisible: !options.selectedAppTab,
    automaticSeen: notificationActivation.automaticSeen,
  }), [
    notificationActivation.automaticSeen, options.activePane?.id, options.activeSessionId,
    options.activeWindowId, options.appFocused, options.currentHostProfileId,
    options.selectedAppTab, options.serverIdentity,
  ]);
  const runtime = useAgentRuntime({
    client: options.agentClient,
    scope,
    focus,
    topologyWindowIds: options.snapshot.windows.map((window) => window.id),
    soundPreferences: options.soundPreferences,
    onStatus: options.setStatus,
  });
  const host = useMemo(() => {
    const identity = agentHostIdentity(scope);
    return scope && identity && options.decisionsArePersistable
      ? { profileId: scope.hostProfileId, identity }
      : undefined;
  }, [options.decisionsArePersistable, scope]);
  const workflow = useAgentWorkflow({
    launchContext: options.activeSession && options.activeWindow && options.activePane && options.activeRoot
      ? {
        sessionId: options.activeSession.id,
        windowId: options.activeWindow.id,
        paneId: options.activePane.id,
        root: options.activeRoot,
      }
      : undefined,
    host,
    onHooksChanged: (action, hostProfileId, hostIdentity) => {
      runtime.refreshSnapshot();
      options.recordDecision(hostProfileId, action === "install" ? "accepted" : "declined");
      if (action === "uninstall") {
        void runtime.removeHostNaming(hostIdentity).catch((cause) => options.setStatus(String(cause)));
      }
    },
    onModalChange: options.setAgentModalOpen,
    onStatus: options.setStatus,
    runtime,
  });
  const hostSetup = useAgentHostSetup({
    adapters: runtime.adapters,
    autoSetup: options.agentAutoSetup,
    applyHooks: runtime.applyHooks,
    applyHostNaming: runtime.applyHostNaming,
    connected: Boolean(scope),
    decision: options.decision,
    decisionsArePersistable: options.decisionsArePersistable,
    hostIdentity: host?.identity,
    hostLabel: options.hostLabel,
    hostProfileId: options.currentHostProfileId,
    onStatus: options.setStatus,
    openReview: workflow.openHookReview,
    recordDecision: options.recordDecision,
    refreshWiring: runtime.refreshSnapshot,
    reviewHooks: runtime.reviewHooks,
  });

  return useMemo(() => ({ hostSetup, notificationActivation, runtime, scope, workflow }), [
    hostSetup, notificationActivation, runtime, scope, workflow,
  ]);
}
