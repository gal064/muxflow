import { StrictMode } from "react";
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentClient } from "./api";
import { displayState } from "./selectors";
import { defaultAgentSoundPreferences, type AgentRecord, type AgentRequestScope, type AgentSnapshot, type AgentWireEvent } from "./types";
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

function Harness({ client, connected = true, focused = true, automaticSeen = true, connectionEpoch = 1, topologyGeneration = 9, topologyWindowIds = ["@1"], effects, onNotificationInstrumentation, onSoundInstrumentation }: { client: AgentClient; connected?: boolean; focused?: boolean; automaticSeen?: boolean; connectionEpoch?: number; topologyGeneration?: number; topologyWindowIds?: string[]; effects?: AgentRuntimeOptions["effects"]; onNotificationInstrumentation?: AgentRuntimeOptions["onNotificationInstrumentation"]; onSoundInstrumentation?: AgentRuntimeOptions["onSoundInstrumentation"] }) {
  const runtime = useAgentRuntime({
    client,
    scopes: connected ? [{ scope: { ...scope, connectionEpoch, topologyGeneration }, topologyWindowIds }] : [],
    shownHostIds: ["local"],
    focus: { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", windowId: "@1", paneId: "%1", appFocused: focused, terminalVisible: true, automaticSeen },
    soundPreferences: defaultAgentSoundPreferences,
    onStatus: vi.fn(),
    effects, onNotificationInstrumentation, onSoundInstrumentation,
  });
  return <output
    data-names={runtime.agents.map((record) => record.displayName).join(",")}
    data-topology-generation={runtime.topologyAuthority?.topologyGeneration}
    data-covered-windows={[...(runtime.topologyAuthority?.coveredWindowIds ?? [])].join(",")}
  >{runtime.agents.map((record) => `${record.id}:${displayState(record)}`).join(",")}</output>;
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

  it("does not let an in-flight seen request suppress the replacement connection", async () => {
    const first: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
      revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9),
      notificationWatermark: agentGeneration(9), authoritative: true, adapters: [],
      agents: [agent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 1 })],
    };
    const replacement = { ...first, connectionEpoch: 2 };
    let resolveFirstSeen!: () => void;
    const firstSeen = new Promise<void>((resolve) => { resolveFirstSeen = resolve; });
    const client = clientFor(first);
    vi.mocked(client.snapshot).mockResolvedValueOnce(first).mockResolvedValue(replacement);
    vi.mocked(client.markSeen).mockImplementationOnce(() => firstSeen).mockResolvedValue(undefined);
    let renderer: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} connectionEpoch={1} />); });
    expect(client.markSeen).toHaveBeenCalledTimes(1);

    await act(async () => renderer!.update(<Harness client={client} connectionEpoch={2} />));
    expect(client.markSeen).toHaveBeenCalledTimes(2);
    expect(client.markSeen).toHaveBeenLastCalledWith(
      { ...scope, connectionEpoch: 2 }, "agent-1", "8",
    );
    await act(async () => resolveFirstSeen());
    await act(async () => renderer!.unmount());
  });

  it("does not claim a new topology is covered until its refresh snapshot is accepted", async () => {
    const first: AgentSnapshot = {
      hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
      revision: agentGeneration(9), eventSequence: agentGeneration(9), acceptedGeneration: agentGeneration(9),
      notificationWatermark: agentGeneration(9), authoritative: true, adapters: [], agents: [],
    };
    let resolveRefresh!: (snapshot: AgentSnapshot) => void;
    const refresh = new Promise<AgentSnapshot>((resolve) => { resolveRefresh = resolve; });
    const client = clientFor(first);
    vi.mocked(client.snapshot).mockResolvedValueOnce(first).mockImplementationOnce(() => refresh);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness client={client} focused={false} topologyGeneration={9} />); });
    expect(renderer.root.findByType("output").props["data-topology-generation"]).toBe(9);

    await act(async () => renderer.update(<Harness
      client={client} focused={false} topologyGeneration={10} topologyWindowIds={["@1", "@2"]}
    />));
    expect(renderer.root.findByType("output").props["data-topology-generation"]).toBe(9);
    await act(async () => resolveRefresh(first));
    expect(renderer.root.findByType("output").props["data-topology-generation"]).toBe(10);
    expect(renderer.root.findByType("output").props["data-covered-windows"]).toBe("@1,@2");
    await act(async () => renderer.unmount());
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

const remoteScope: AgentRequestScope = { clientId: "client-remote", hostProfileId: "remote", serverIdentity: "server-r", topologyGeneration: 3, connectionEpoch: 1 };
const remoteAgent = (overrides: Parameters<typeof agent>[0] = {}) => agent({ hostProfileId: "remote", serverIdentity: "server-r", ...overrides });

function snapshotOf(target: AgentRequestScope, agents: AgentRecord[], revision = 3): AgentSnapshot {
  return {
    hostProfileId: target.hostProfileId, serverIdentity: target.serverIdentity, connectionEpoch: target.connectionEpoch,
    revision: agentGeneration(revision), eventSequence: agentGeneration(revision), acceptedGeneration: agentGeneration(revision),
    notificationWatermark: agentGeneration(revision), authoritative: true, adapters: [], agents,
  };
}

function multiHostClient(snapshots: Readonly<Record<string, AgentSnapshot>>) {
  const client = clientFor(snapshots.local);
  client.snapshot = vi.fn(async (target: AgentRequestScope) => snapshots[target.hostProfileId]);
  return client;
}

interface MultiHostProps {
  client: ReturnType<typeof clientFor>;
  /** Hosts with a live scope; every host is shown unless `shown` says otherwise. */
  hosts?: readonly string[];
  shown?: readonly string[];
  focusHost?: string;
  focused?: boolean;
  remoteEpoch?: number;
  effects?: AgentRuntimeOptions["effects"];
  onNotificationInstrumentation?: AgentRuntimeOptions["onNotificationInstrumentation"];
  observe?(runtime: ReturnType<typeof useAgentRuntime>): void;
}

function MultiHost({ client, hosts = ["local", "remote"], shown = hosts, focusHost = "local", focused = true, remoteEpoch = 1, effects, onNotificationInstrumentation, observe }: MultiHostProps) {
  const runtime = useAgentRuntime({
    client,
    scopes: hosts.map((hostProfileId) => ({
      scope: hostProfileId === "local" ? scope : { ...remoteScope, connectionEpoch: remoteEpoch },
      topologyWindowIds: ["@1"],
    })),
    shownHostIds: shown,
    focus: {
      hostProfileId: focusHost, serverIdentity: focusHost === "local" ? "server-a" : "server-r",
      sessionId: "$1", windowId: "@1", paneId: "%1", appFocused: focused, terminalVisible: true, automaticSeen: true,
    },
    soundPreferences: defaultAgentSoundPreferences,
    onStatus: vi.fn(),
    effects, onNotificationInstrumentation,
  });
  observe?.(runtime);
  const names = (hostProfileId: string) => runtime.byHost.get(hostProfileId)?.agents.map((record) => `${record.id}:${record.displayName}`).join(",") ?? "-";
  return <output
    data-local={names("local")} data-remote={names("remote")}
    data-active={runtime.agents.map((record) => record.displayName).join(",")}
    data-authoritative={String(runtime.state.authoritative)}
    data-topology-generation={runtime.topologyAuthority?.topologyGeneration}
  />;
}

describe("useAgentRuntime across hosts", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  const twoHosts = () => multiHostClient({
    local: snapshotOf(scope, [agent({ displayName: "Local one" })]),
    remote: snapshotOf(remoteScope, [remoteAgent({ displayName: "Remote one" })]),
  });

  it("requests one snapshot per scope and projects every host", async () => {
    const client = twoHosts();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} />); });
    expect(client.snapshot).toHaveBeenCalledTimes(2);
    expect(client.snapshot).toHaveBeenCalledWith(scope);
    expect(client.snapshot).toHaveBeenCalledWith(remoteScope);
    const output = renderer.root.findByType("output");
    expect(output.props["data-local"]).toBe("agent-1:Local one");
    expect(output.props["data-remote"]).toBe("agent-1:Remote one");
    expect(output.props["data-active"]).toBe("Local one");
    await act(async () => renderer.unmount());
  });

  it("notifies and plays a sound for a blocked agent on a host the user is not looking at", async () => {
    const client = twoHosts();
    const emitNotification = vi.fn(async () => ({ id: 1, actionable: true }));
    const playSound = vi.fn(async () => undefined);
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} effects={{ emitNotification, playSound }} />); });
    // Same pane id as the focused pane, on the other host: pane ids repeat
    // across tmux servers, so the focus rule has to look at the host too.
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "remote", serverIdentity: "server-r", connectionEpoch: 1, sequence: agentGeneration(4),
      record: remoteAgent({ lifecycle: "blocked", lifecycleGeneration: 4, attentionGeneration: 4 }),
    }));
    expect(emitNotification).toHaveBeenCalledTimes(1);
    expect(emitNotification).toHaveBeenCalledWith(expect.objectContaining({
      event: "blocked", route: expect.objectContaining({ hostProfileId: "remote", serverIdentity: "server-r" }),
    }));
    expect(playSound).toHaveBeenCalledWith("blocked", defaultAgentSoundPreferences, expect.any(Function));
    await act(async () => renderer.unmount());
  });

  it("suppresses the same transition on the focused host's focused pane", async () => {
    const client = twoHosts();
    const emitNotification = vi.fn(async () => ({ id: 1, actionable: true }));
    const playSound = vi.fn(async () => undefined);
    const instrumentation = vi.fn();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} effects={{ emitNotification, playSound }} onNotificationInstrumentation={instrumentation} />); });
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(4),
      record: agent({ lifecycle: "blocked", lifecycleGeneration: 4, attentionGeneration: 4 }),
    }));
    expect(emitNotification).not.toHaveBeenCalled();
    expect(playSound).not.toHaveBeenCalled();
    expect(instrumentation).toHaveBeenCalledWith(expect.objectContaining({ outcome: "suppressed-focused" }));
    await act(async () => renderer.unmount());
  });

  it("marks seen only on the active host", async () => {
    const client = multiHostClient({
      local: snapshotOf(scope, [agent({ lifecycle: "idle", attentionGeneration: 3, seenGeneration: 1 })]),
      remote: snapshotOf(remoteScope, [remoteAgent({ lifecycle: "idle", attentionGeneration: 3, seenGeneration: 1 })]),
    });
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} />); });
    expect(client.markSeen).toHaveBeenCalledTimes(1);
    expect(client.markSeen).toHaveBeenCalledWith(scope, "agent-1", "3");
    await act(async () => renderer.update(<MultiHost client={client} focusHost="remote" />));
    expect(client.markSeen).toHaveBeenCalledTimes(2);
    expect(client.markSeen).toHaveBeenLastCalledWith(remoteScope, "agent-1", "3");
    await act(async () => renderer.unmount());
  });

  it("disconnects only the host whose scope vanished and re-requests only that host", async () => {
    const client = twoHosts();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} focusHost="remote" />); });
    expect(renderer.root.findByType("output").props["data-authoritative"]).toBe("true");
    expect(renderer.root.findByType("output").props["data-topology-generation"]).toBe(3);
    await act(async () => renderer.update(<MultiHost client={client} focused={false} focusHost="remote" hosts={["local"]} shown={["local", "remote"]} />));
    const output = renderer.root.findByType("output");
    expect(output.props["data-authoritative"]).toBe("false");
    expect(output.props["data-topology-generation"]).toBeUndefined();
    expect(output.props["data-active"]).toBe("");
    expect(output.props["data-local"]).toBe("agent-1:Local one");
    await act(async () => renderer.update(<MultiHost client={client} focused={false} focusHost="remote" remoteEpoch={2} />));
    expect(client.snapshot).toHaveBeenCalledTimes(3);
    expect(client.snapshot).toHaveBeenLastCalledWith({ ...remoteScope, connectionEpoch: 2 });
    expect(vi.mocked(client.snapshot).mock.calls.filter(([target]) => target.hostProfileId === "local")).toHaveLength(1);
    await act(async () => renderer.unmount());
  });

  it("drops a host that is no longer shown and ignores its stragglers", async () => {
    const client = twoHosts();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} focusHost="remote" />); });
    await act(async () => renderer.update(<MultiHost client={client} focused={false} focusHost="remote" hosts={["local"]} shown={["local"]} />));
    await act(async () => client.publish({
      kind: "snapshot", snapshot: snapshotOf(remoteScope, [remoteAgent({ displayName: "Straggler" })], 5),
    }));
    const output = renderer.root.findByType("output");
    expect(output.props["data-authoritative"]).toBe("false");
    expect(output.props["data-active"]).toBe("");
    expect(output.props["data-local"]).toBe("agent-1:Local one");
    await act(async () => renderer.unmount());
  });

  it("notifies the same agent id at the same generation on each host", async () => {
    const client = twoHosts();
    const emitNotification = vi.fn(async () => ({ id: 1, actionable: true }));
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} effects={{ emitNotification, playSound: vi.fn(async () => undefined) }} />); });
    for (const [hostProfileId, serverIdentity, record] of [
      ["local", "server-a", agent({ lifecycle: "blocked", lifecycleGeneration: 4, attentionGeneration: 4 })],
      ["remote", "server-r", remoteAgent({ lifecycle: "blocked", lifecycleGeneration: 4, attentionGeneration: 4 })],
    ] as const) {
      await act(async () => client.publish({ kind: "upsert", hostProfileId, serverIdentity, connectionEpoch: 1, sequence: agentGeneration(4), record }));
    }
    expect(emitNotification).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  it("refreshes only the host it is asked about", async () => {
    const client = twoHosts();
    let latest!: ReturnType<typeof useAgentRuntime>;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} observe={(runtime) => { latest = runtime; }} />); });
    expect(client.snapshot).toHaveBeenCalledTimes(2);
    await act(async () => latest.refreshSnapshot("remote"));
    expect(client.snapshot).toHaveBeenCalledTimes(3);
    expect(client.snapshot).toHaveBeenLastCalledWith(remoteScope);
    await act(async () => latest.refreshSnapshot());
    expect(client.snapshot).toHaveBeenCalledTimes(4);
    expect(client.snapshot).toHaveBeenLastCalledWith(scope);
    await act(async () => renderer.unmount());
  });

  it("requests again after a StrictMode remount instead of keeping a cancelled request", async () => {
    const client = twoHosts();
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<StrictMode><MultiHost client={client} focused={false} /></StrictMode>); });
    const output = renderer.root.findByType("output");
    expect(output.props["data-local"]).toBe("agent-1:Local one");
    expect(output.props["data-remote"]).toBe("agent-1:Remote one");
    await act(async () => renderer.unmount());
  });

  it("keeps another host's projection identity across one host's event", async () => {
    const client = twoHosts();
    const observed: Array<ReturnType<typeof useAgentRuntime>["byHost"]> = [];
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<MultiHost client={client} focused={false} observe={(runtime) => observed.push(runtime.byHost)} />); });
    const before = observed[observed.length - 1];
    await act(async () => client.publish({
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(4),
      record: agent({ displayName: "Local renamed" }),
    }));
    const after = observed[observed.length - 1];
    expect(after).not.toBe(before);
    expect(after.get("remote")).toBe(before.get("remote"));
    expect(after.get("local")).not.toBe(before.get("local"));
    expect(after.get("local")?.agents[0].displayName).toBe("Local renamed");
    await act(async () => renderer.unmount());
  });
});
