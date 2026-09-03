import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentClient } from "./api";
import { agentGeneration } from "./generation";
import { buildAgentRows, jumpTarget, unreadCount } from "./agentsList";
import { agent } from "./testFixtures";
import { useAgentRuntime } from "./useAgentRuntime";
import { defaultAgentSoundPreferences, type AgentRequestScope, type AgentSnapshot, type AgentWireEvent } from "./types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({ id: 1, actionable: true })) }));

const scope: AgentRequestScope = { clientId: "client", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 9, connectionEpoch: 1 };

const emptySnapshot: AgentSnapshot = {
  hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
  revision: agentGeneration(2), eventSequence: agentGeneration(2), acceptedGeneration: agentGeneration(2),
  notificationWatermark: agentGeneration(2), authoritative: true, adapters: [],
  agents: [
    agent({ id: "quiet", lifecycle: "working", lifecycleGeneration: 2 }),
    agent({ id: "asks", lifecycle: "working", lifecycleGeneration: 2, paneId: "%2", windowId: "@2" }),
  ],
};

/**
 * What the daemon persisted while the desktop was gone: one turn finished
 * without being looked at, one agent blocked. Neither transition was ever
 * delivered as a live event to this client.
 */
const afterTheGap: AgentSnapshot = {
  ...emptySnapshot,
  revision: agentGeneration(7), eventSequence: agentGeneration(7), acceptedGeneration: agentGeneration(7),
  notificationWatermark: agentGeneration(7), connectionEpoch: 2,
  agents: [
    agent({ id: "quiet", lifecycle: "idle", lifecycleGeneration: 6, attentionGeneration: 5, attentionKind: "completed", seenGeneration: 0 }),
    agent({ id: "asks", lifecycle: "blocked", lifecycleGeneration: 7, attentionGeneration: 6, attentionKind: "blocked", seenGeneration: 0, paneId: "%2", windowId: "@2" }),
  ],
};

function client(): AgentClient {
  const listeners = new Set<(event: AgentWireEvent) => void>();
  return {
    snapshot: vi.fn<AgentClient["snapshot"]>()
      .mockResolvedValueOnce(emptySnapshot)
      .mockResolvedValue(afterTheGap),
    launch: vi.fn(), resume: vi.fn(), rename: vi.fn(), markSeen: vi.fn(async () => undefined),
    reviewHooks: vi.fn(), applyHooks: vi.fn(), applyHostNaming: vi.fn(async () => "alreadyCurrent" as const), publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn(),
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
}

function Sidebar({ client: agentClient, connected, connectionEpoch }: { client: AgentClient; connected: boolean; connectionEpoch: number }) {
  const runtime = useAgentRuntime({
    client: agentClient,
    scope: connected ? { ...scope, connectionEpoch } : undefined,
    topologyWindowIds: ["@1", "@2"],
    // Deliberately not focused on either pane: catch-up must not be quietly
    // consumed by the same render that displays it.
    focus: { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@9", paneId: "%9", appFocused: false, terminalVisible: true, automaticSeen: true },
    soundPreferences: { ...defaultAgentSoundPreferences, enabled: false },
    onStatus: vi.fn(),
    effects: { emitNotification: vi.fn(async () => ({ id: 1, actionable: true })), playSound: vi.fn(async () => undefined) },
  });
  const rows = buildAgentRows(runtime.agents, () => ({ workspaceOrder: 0, workspaceName: "work" }), () => true, "status");
  return <output
    data-jump={jumpTarget(rows)?.agent.id ?? ""}
    data-unread={unreadCount(rows)}
  >{rows.map((row) => `${row.agent.id}:${row.state}`).join(",")}</output>;
}

/**
 * Phase 13.3.1: everything that happened while the app was away has to be
 * visible the moment it comes back, and it has to come from the host's own
 * persisted state rather than from events this client happened to witness.
 */
describe("catch-up after a disconnect", () => {
  beforeEach(() => { Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true }); });

  it("lights the rows and the unread count from the reconnect snapshot alone", async () => {
    const agentClient = client();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Sidebar client={agentClient} connected connectionEpoch={1} />); });
    const before = renderer.root.findByType("output");
    expect(before.children.join("").split(",").sort()).toEqual(["asks:working", "quiet:working"]);
    expect(before.props["data-unread"]).toBe(0);

    // Away. Nothing is delivered; the host is where the turns actually happen.
    await act(async () => renderer.update(<Sidebar client={agentClient} connected={false} connectionEpoch={1} />));
    expect(agentClient.snapshot).toHaveBeenCalledTimes(1);

    await act(async () => renderer.update(<Sidebar client={agentClient} connected connectionEpoch={2} />));
    const after = renderer.root.findByType("output");
    // Blocked outranks done-unread; both are unread, and ⌘⇧U goes to the
    // loudest of them.
    expect(after.children.join("")).toBe("asks:blocked,quiet:done");
    expect(after.props["data-unread"]).toBe(2);
    expect(after.props["data-jump"]).toBe("asks");
    expect(agentClient.markSeen).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });
});
