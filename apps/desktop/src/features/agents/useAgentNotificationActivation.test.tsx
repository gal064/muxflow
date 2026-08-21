import { act, create } from "react-test-renderer";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Pane, TmuxSnapshot } from "../../app/types";
import type { AgentClient } from "./api";
import { agentGeneration } from "./generation";
import type { AgentRequestScope } from "./types";
import { defaultAgentSoundPreferences } from "./types";
import { agent } from "./testFixtures";
import type { PaneSurfaceResult } from "../../app/useShellNavigation";
import { useAgentNotificationActivation } from "./useAgentNotificationActivation";
import { useAgentRuntime } from "./useAgentRuntime";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

const pane = (id: string): Pane => ({
  id, sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24,
  left: 0, top: 0, currentPath: "/repo", currentCommand: "bash",
});
const snapshot = (paneId: string): TmuxSnapshot => ({
  sessions: [{ id: "$1", name: "work", windowCount: 1, attachedClients: 0 }],
  windows: [{ id: "@1", sessionId: "$1", index: 0, name: "agent", active: true, layout: "" }],
  panes: [pane(paneId)],
});
const route = {
  hostProfile: "local", serverIdentity: "server-a", sessionId: "$1", sessionName: "work",
  windowId: "@1", windowName: "agent", paneId: "%old", agentId: "clicked", attentionGeneration: agentGeneration("18446744073709551615"),
};

function client(): AgentClient {
  return {
    snapshot: vi.fn(), launch: vi.fn(), resume: vi.fn(), rename: vi.fn(), markSeen: vi.fn(async () => undefined),
    reviewHooks: vi.fn(), applyHooks: vi.fn(), applyHostNaming: vi.fn(async () => "alreadyCurrent" as const),
    publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn(), subscribe: vi.fn(() => () => undefined),
  };
}

interface HarnessProps {
  agentClient: AgentClient;
  agentScope: AgentRequestScope;
  connectionEpoch: number;
  snapshot: TmuxSnapshot;
  requestReconnect(): void;
  surfacePaneDestination(target: Pane, source: string, successMessage?: string): Promise<PaneSurfaceResult>;
}

let activation: ReturnType<typeof useAgentNotificationActivation>;
function Harness(props: HarnessProps) {
  activation = useAgentNotificationActivation({
    ...props, connected: true, currentHostProfileId: "local", focusedPaneId: undefined, profiles: [],
    setStatus: vi.fn(), switchHostProfile: vi.fn(),
  });
  return null;
}

let guardedActivation: ReturnType<typeof useAgentNotificationActivation>;
function FocusCallbackHarness({ agentClient }: { agentClient: AgentClient }) {
  const [focusedPaneId, setFocusedPaneId] = useState<string>();
  const [appFocused, setAppFocused] = useState(false);
  guardedActivation = useAgentNotificationActivation({
    agentClient,
    agentScope: { ...routeScope },
    connected: true,
    connectionEpoch: routeScope.connectionEpoch,
    currentHostProfileId: "local",
    focusedPaneId,
    profiles: [],
    snapshot: snapshot("%1"),
    requestReconnect: vi.fn(),
    setStatus: vi.fn(),
    switchHostProfile: vi.fn(),
    surfacePaneDestination: async (target) => {
      // This is the same state change caused by TerminalPane's real onFocus
      // callback. It must not cancel the exact-notification guard.
      setFocusedPaneId(target.id);
      setAppFocused(true);
      return { ok: true };
    },
  });
  useAgentRuntime({
    client: agentClient,
    scope: routeScope,
    topologyWindowIds: ["@1"],
    focus: {
      hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@1",
      paneId: focusedPaneId, appFocused, terminalVisible: true, automaticSeen: guardedActivation.automaticSeen,
    },
    soundPreferences: defaultAgentSoundPreferences,
    onStatus: vi.fn(),
  });
  return null;
}

const routeScope: AgentRequestScope = {
  clientId: "client-1", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 7, connectionEpoch: 1,
};

describe("notification activation orchestration", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("refreshes once after stale focus but never focuses or acknowledges a replacement pane", async () => {
    invokeMock
      .mockResolvedValueOnce({ resolution: "exact", sessionId: "$1", windowId: "@1", paneId: "%old", attentionGeneration: route.attentionGeneration })
      .mockResolvedValueOnce({ resolution: "expired" });
    const agentClient = client();
    const reconnect = vi.fn();
    const surface = vi.fn()
      .mockResolvedValueOnce({ ok: false, error: new Error("stale topology generation") });
    const firstScope = { clientId: "client-1", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 7, connectionEpoch: 1 };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness agentClient={agentClient} agentScope={firstScope} connectionEpoch={1} requestReconnect={reconnect} snapshot={snapshot("%old")} surfacePaneDestination={surface} />); });
    await act(async () => { expect(await activation.activateNotificationRoute(route)).toBe(false); });
    expect(reconnect).toHaveBeenCalledTimes(1);
    const freshScope = { ...firstScope, clientId: "client-2", topologyGeneration: 8, connectionEpoch: 2 };
    await act(async () => { renderer!.update(<Harness agentClient={agentClient} agentScope={freshScope} connectionEpoch={2} requestReconnect={reconnect} snapshot={snapshot("%survivor")} surfacePaneDestination={surface} />); });
    expect(surface).toHaveBeenCalledTimes(1);
    expect(surface).toHaveBeenLastCalledWith(expect.objectContaining({ id: "%old" }), "Notification");
    expect(agentClient.markSeen).not.toHaveBeenCalled();
    expect(reconnect).toHaveBeenCalledTimes(1);
    await act(async () => renderer!.unmount());
  });

  it("keeps the exact activation guard through the terminal focus callback and acknowledgement", async () => {
    invokeMock.mockResolvedValue({ resolution: "exact", sessionId: "$1", windowId: "@1", paneId: "%1", attentionGeneration: route.attentionGeneration });
    let resolveExact: (() => void) | undefined;
    const exactAck = new Promise<void>((resolve) => { resolveExact = resolve; });
    const agentClient = client();
    vi.mocked(agentClient.snapshot).mockResolvedValue({
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
      revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9),
      notificationWatermark: agentGeneration(9), authoritative: true, adapters: [],
      agents: [
        agent({ id: "clicked", paneId: "%1", attentionGeneration: route.attentionGeneration, seenGeneration: 1 }),
        agent({ id: "same-pane", paneId: "%1", attentionGeneration: 9, seenGeneration: 1 }),
      ],
    });
    vi.mocked(agentClient.markSeen).mockImplementation(async (_scope, agentId) => {
      if (agentId === "clicked" && vi.mocked(agentClient.markSeen).mock.calls.length === 1) await exactAck;
    });

    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<FocusCallbackHarness agentClient={agentClient} />); });
    let activated: Promise<boolean> | undefined;
    await act(async () => {
      activated = guardedActivation.activateNotificationRoute(route);
      await Promise.resolve();
    });
    expect(agentClient.markSeen).toHaveBeenCalledTimes(1);
    expect(agentClient.markSeen).toHaveBeenCalledWith(routeScope, "clicked", route.attentionGeneration);

    await act(async () => {
      resolveExact?.();
      await activated;
    });
    expect(vi.mocked(agentClient.markSeen).mock.calls.some(([, agentId]) => agentId === "same-pane")).toBe(true);
    await act(async () => renderer!.unmount());
  });

  it("refuses an unmapped click before topology fallback, focus, or seen acknowledgement", async () => {
    const agentClient = client();
    const reconnect = vi.fn();
    const surface = vi.fn(async () => ({ ok: true }));
    const scope = { ...routeScope };
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness agentClient={agentClient} agentScope={scope} connectionEpoch={1} requestReconnect={reconnect} snapshot={snapshot("%arbitrary")} surfacePaneDestination={surface} />); });
    await act(async () => {
      expect(await activation.activateNotificationRoute({ ...route, sessionId: "", windowId: "", paneId: "" })).toBe(false);
    });
    expect(invokeMock).not.toHaveBeenCalled();
    expect(surface).not.toHaveBeenCalled();
    expect(agentClient.markSeen).not.toHaveBeenCalled();
    expect(reconnect).not.toHaveBeenCalled();
    await act(async () => renderer!.unmount());
  });
});
