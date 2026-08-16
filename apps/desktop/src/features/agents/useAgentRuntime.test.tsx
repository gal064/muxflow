import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentClient } from "./api";
import { displayState } from "./selectors";
import { defaultAgentSoundPreferences, type AgentRequestScope, type AgentSnapshot, type AgentWireEvent } from "./types";
import { agent } from "./testFixtures";
import { useAgentRuntime, type AgentRuntimeOptions } from "./useAgentRuntime";
import { agentGeneration } from "./generation";

const invokeMock = vi.hoisted(() => vi.fn(async () => ({ id: 1, actionable: true })));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const scope: AgentRequestScope = { clientId: "client", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 9, connectionEpoch: 1 };

function clientFor(snapshot: AgentSnapshot): AgentClient & { markSeen: ReturnType<typeof vi.fn>; publish(event: AgentWireEvent): void } {
  const listeners = new Set<(event: AgentWireEvent) => void>();
  return {
    snapshot: vi.fn(async () => snapshot), launch: vi.fn(), resume: vi.fn(), rename: vi.fn(),
    markSeen: vi.fn(async () => undefined), reviewHooks: vi.fn(), applyHooks: vi.fn(), applyHostNaming: vi.fn(async () => "alreadyCurrent" as const),
    publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn(),
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    publish(event) { for (const listener of listeners) listener(event); },
  };
}

function Harness({ client, connected = true, focused = true, automaticSeen = true, connectionEpoch = 1, topologyGeneration = 9, effects, onNotificationInstrumentation, onSoundInstrumentation }: { client: AgentClient; connected?: boolean; focused?: boolean; automaticSeen?: boolean; connectionEpoch?: number; topologyGeneration?: number; effects?: AgentRuntimeOptions["effects"]; onNotificationInstrumentation?: AgentRuntimeOptions["onNotificationInstrumentation"]; onSoundInstrumentation?: AgentRuntimeOptions["onSoundInstrumentation"] }) {
  const runtime = useAgentRuntime({
    client, scope: connected ? { ...scope, connectionEpoch, topologyGeneration } : undefined,
    focus: { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@1", paneId: "%1", appFocused: focused, terminalVisible: true, automaticSeen },
    soundPreferences: defaultAgentSoundPreferences,
    onStatus: vi.fn(),
    effects, onNotificationInstrumentation, onSoundInstrumentation,
  });
  return <output data-names={runtime.agents.map((record) => record.displayName).join(",")}>{runtime.agents.map((record) => `${record.id}:${displayState(record)}`).join(",")}</output>;
}

describe("useAgentRuntime focus semantics", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("treats reconnect snapshot generations as replay and marks only the focused pane's exact unseen generation", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(8), eventSequence: agentGeneration(8), acceptedGeneration: agentGeneration(8), notificationWatermark: agentGeneration(8), authoritative: true, adapters: [],
      agents: [agent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 3 })],
    };
    const client = clientFor(snapshot);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} />); });
    expect(client.markSeen).toHaveBeenCalledTimes(1);
    expect(client.markSeen).toHaveBeenCalledWith(scope, "agent-1", "8");
    expect(renderer!.root.findByType("output").children.join("")).toBe("agent-1:idle");
    expect(invokeMock).not.toHaveBeenCalled();
    await act(async () => renderer!.unmount());
  });

  it("emits an offline working-to-idle completion once from its persisted cause and consumes the reconnect watermark", async () => {
    const before: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(10), eventSequence: agentGeneration(10), acceptedGeneration: agentGeneration(10), notificationWatermark: agentGeneration(10), authoritative: true, adapters: [],
      agents: [agent({ lifecycle: "idle", lifecycleGeneration: 10, attentionGeneration: 3, attentionKind: "completed", seenGeneration: 3 })],
    };
    const after: AgentSnapshot = {
      ...before, revision: agentGeneration(12), eventSequence: agentGeneration(12), acceptedGeneration: agentGeneration(12), notificationWatermark: agentGeneration(12),
      agents: [agent({ lifecycle: "idle", lifecycleGeneration: 12, attentionGeneration: 12, attentionKind: "completed", seenGeneration: 3 })],
    };
    const client = clientFor(before);
    vi.mocked(client.snapshot).mockResolvedValueOnce(before).mockResolvedValue(after);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} focused={false} />); });
    await act(async () => renderer!.update(<Harness client={client} connected={false} focused={false} />));
    await act(async () => renderer!.update(<Harness client={client} focused={false} />));
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("emit_agent_notification", expect.objectContaining({
      notification: expect.objectContaining({ title: expect.stringContaining("finished") }),
    }));
    await act(async () => renderer!.update(<Harness client={client} connected={false} focused={false} />));
    await act(async () => renderer!.update(<Harness client={client} focused={false} />));
    expect(invokeMock).toHaveBeenCalledTimes(1);
    await act(async () => renderer!.unmount());
  });

  it("suppresses ordinary pane-wide seen marking during exact notification activation", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9), notificationWatermark: agentGeneration(9), authoritative: true, adapters: [],
      agents: [agent({ id: "clicked", attentionGeneration: 8, seenGeneration: 1 }), agent({ id: "same-pane", attentionGeneration: 9, seenGeneration: 1 })],
    };
    const client = clientFor(snapshot);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness automaticSeen={false} client={client} />); });
    expect(client.markSeen).not.toHaveBeenCalled();
    await act(async () => renderer!.unmount());
  });

  it("refreshes an authoritative agent snapshot when only the bridge connection epoch changes", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9), notificationWatermark: agentGeneration(9), authoritative: true, adapters: [], agents: [agent()],
    };
    const client = clientFor(snapshot);
    vi.mocked(client.snapshot).mockResolvedValueOnce(snapshot).mockResolvedValue({ ...snapshot, connectionEpoch: 2 });
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} connectionEpoch={1} focused={false} />); });
    await act(async () => renderer!.update(<Harness client={client} connectionEpoch={2} focused={false} />));
    expect(client.snapshot).toHaveBeenCalledTimes(2);
    expect(client.snapshot).toHaveBeenLastCalledWith({ ...scope, connectionEpoch: 2 });
    await act(async () => renderer!.unmount());
  });

  it("keeps a live event that arrives before an older reconnect snapshot response", async () => {
    const before: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(5), eventSequence: agentGeneration(5), acceptedGeneration: agentGeneration(5), notificationWatermark: agentGeneration(5), authoritative: true, adapters: [], agents: [agent({ displayName: "Before" })],
    };
    const stale: AgentSnapshot = {
      ...before, revision: agentGeneration(6), eventSequence: agentGeneration(6), acceptedGeneration: agentGeneration(6),
      agents: [agent({ displayName: "Stale reconnect" })],
    };
    let resolveReconnect!: (snapshot: AgentSnapshot) => void;
    const reconnect = new Promise<AgentSnapshot>((resolve) => { resolveReconnect = resolve; });
    const client = clientFor(before);
    vi.mocked(client.snapshot).mockResolvedValueOnce(before).mockImplementationOnce(() => reconnect);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} connectionEpoch={1} focused={false} />); });
    await act(async () => renderer!.update(<Harness client={client} connectionEpoch={1} focused={false} topologyGeneration={10} />));
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(7),
      record: agent({ displayName: "Live wins" }),
    }));
    await act(async () => resolveReconnect(stale));
    expect(renderer!.root.findByType("output").props["data-names"]).toBe("Live wins");
    await act(async () => renderer!.unmount());
  });

  it("consumes a seen-generation broadcast produced by another desktop client", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9), notificationWatermark: agentGeneration(9), authoritative: true, adapters: [],
      agents: [agent({ lifecycle: "idle", attentionGeneration: 8, attentionKind: "completed", seenGeneration: 1 })],
    };
    const client = clientFor(snapshot);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} focused={false} />); });
    expect(renderer!.root.findByType("output").children.join("")).toBe("agent-1:done");
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(10),
      record: agent({ lifecycle: "idle", lifecycleGeneration: 3, attentionGeneration: 8, attentionKind: "completed", seenGeneration: 8 }),
    }));
    expect(renderer!.root.findByType("output").children.join("")).toBe("agent-1:idle");
    await act(async () => renderer!.unmount());
  });

  it("applies a rename event published to a second desktop client", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9), notificationWatermark: agentGeneration(9), authoritative: true, adapters: [], agents: [agent()],
    };
    const client = clientFor(snapshot);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} focused={false} />); });
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(10),
      record: agent({ displayName: "Renamed elsewhere" }),
    }));
    expect(renderer!.root.findByType("output").props["data-names"]).toBe("Renamed elsewhere");
    await act(async () => renderer!.unmount());
  });

  it("exposes injectable notification and sound instrumentation for acceptance testing", async () => {
    const snapshot: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, revision: agentGeneration(3), eventSequence: agentGeneration(3), acceptedGeneration: agentGeneration(3), notificationWatermark: agentGeneration(3), authoritative: true, adapters: [], agents: [agent()],
    };
    const client = clientFor(snapshot);
    const emitNotification = vi.fn(async () => ({ id: 1, actionable: false }));
    const playSound = vi.fn(async (_event, _preferences, instrument) => instrument({ event: "blocked", outcome: "played" }));
    const notificationInstrumentation = vi.fn();
    const soundInstrumentation = vi.fn();
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} effects={{ emitNotification, playSound }} focused={false} onNotificationInstrumentation={notificationInstrumentation} onSoundInstrumentation={soundInstrumentation} />); });
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(4),
      record: agent({ lifecycle: "blocked", lifecycleGeneration: 4, attentionGeneration: 4 }),
    }));
    expect(emitNotification).toHaveBeenCalledTimes(1);
    expect(notificationInstrumentation).toHaveBeenCalledWith(expect.objectContaining({ outcome: "emitted", actionable: false }));
    expect(soundInstrumentation).toHaveBeenCalledWith({ event: "blocked", outcome: "played" });
    await act(async () => renderer!.unmount());
  });
});
