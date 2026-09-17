import { describe, expect, it } from "vitest";
import { acknowledgeSeen, agentReducer, initialAgentState, initialHostAgentState } from "./state";
import type { AgentRecord, AgentSnapshot, AgentStoreState, HostAgentState } from "./types";
import { agent } from "./testFixtures";
import { agentGeneration } from "./generation";
import { deriveAgentRollups } from "./selectors";

const snapshot = (agents: AgentRecord[], revision = 4, hostProfileId = "local"): AgentSnapshot => ({
  hostProfileId, serverIdentity: "server-a", revision: agentGeneration(revision), eventSequence: agentGeneration(revision),
  connectionEpoch: 1,
  acceptedGeneration: agentGeneration(revision),
  notificationWatermark: agentGeneration(revision), authoritative: true, agents, adapters: [],
});

const local = (state: AgentStoreState): HostAgentState => state.byHost.local ?? initialHostAgentState;

describe("agentReducer", () => {
  it("replaces reconnect state with an authoritative snapshot without merging stale agents", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const disconnected = agentReducer(first, { type: "disconnect", hostProfileId: "local" });
    expect(local(disconnected).authoritative).toBe(false);
    const replaced = agentReducer(disconnected, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([], 5), replayed: true } });
    expect(local(replaced).authoritative).toBe(true);
    expect(local(replaced).byId).toEqual({});
  });

  it("rejects stale, cross-server, and weaker lifecycle generations", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const weaker = agent({ lifecycle: "unknown", lifecycleGeneration: 2, updatedAt: 120 });
    const state = agentReducer(first, { type: "wire", event: { kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5), record: weaker } });
    expect(local(state).byId["agent-1"].lifecycle).toBe("working");
    expect(agentReducer(state, { type: "wire", event: { kind: "removed", hostProfileId: "local", serverIdentity: "server-b", connectionEpoch: 1, sequence: agentGeneration(6), agentId: "agent-1", updatedAt: 130 } })).toBe(state);
    expect(agentReducer(state, { type: "wire", event: { kind: "removed", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5), agentId: "agent-1", updatedAt: 130 } })).toBe(state);
  });

  // The host says "this agent's process is gone" with retirements and no
  // record. Nothing else in the protocol carries that, so dropping the event
  // for want of a record leaves a killed agent's row on screen until some
  // unrelated snapshot happens to follow.
  it("applies a retirement that carries no surviving record", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const retired = agentReducer(first, { type: "wire", event: {
      kind: "retired", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
      sequence: agentGeneration(5), retiredAgentIds: ["agent-1"],
    } });
    expect(local(retired).byId["agent-1"]).toBeUndefined();
    expect(local(retired).eventSequence).toEqual(agentGeneration(5));
  });

  it("ignores a retirement from another server and one for an agent it never had", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const foreign = agentReducer(first, { type: "wire", event: {
      kind: "retired", hostProfileId: "local", serverIdentity: "server-b", connectionEpoch: 1,
      sequence: agentGeneration(5), retiredAgentIds: ["agent-1"],
    } });
    expect(foreign).toBe(first);
    const unknown = agentReducer(first, { type: "wire", event: {
      kind: "retired", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1,
      sequence: agentGeneration(5), retiredAgentIds: ["never-existed"],
    } });
    expect(local(unknown).byId["agent-1"]).toBeDefined();
    expect(local(unknown).eventSequence).toEqual(agentGeneration(5));
  });

  it("atomically retires a manually detected identity when the native identity arrives", () => {
    const manual = agent({ id: "manual", nativeSessionId: "", detectedManually: true });
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([manual]) } });
    const native = agent({ id: "native", nativeSessionId: "session-1", lifecycleGeneration: 5 });
    const promoted = agentReducer(first, { type: "wire", event: {
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5),
      record: native, retiredAgentIds: ["manual"],
    } });
    expect(Object.keys(local(promoted).byId)).toEqual(["native"]);
  });

  it("retains an unmapped record without inventing topology", () => {
    const unmapped = agent({ sessionId: "", windowId: "", paneId: "" });
    const state = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([unmapped]) } });
    expect(local(state).byId[unmapped.id]).toMatchObject({ paneId: "", sessionId: "", windowId: "" });
    const rollups = deriveAgentRollups(Object.values(local(state).byId));
    expect(rollups.byPane.size).toBe(0);
    expect(rollups.byWindow.size).toBe(0);
    expect(rollups.byWorkspace.size).toBe(0);
  });

  it("acknowledges only the exact current attention generation", () => {
    const current = local(agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 3 })]) } }));
    expect(acknowledgeSeen(current, "agent-1", agentGeneration(7))).toBe(current);
    expect(acknowledgeSeen(current, "agent-1", agentGeneration(8)).byId["agent-1"].seenGeneration).toBe("8");
  });

  it("merges an overtaken acknowledgement timestamp without lowering the event barrier", () => {
    const completed = agent({
      lifecycle: "idle",
      attentionKind: "completed",
      attentionGeneration: 8,
      seenGeneration: 3,
      lifecycleChangedAt: 100,
    });
    const initial = agentReducer(initialAgentState, {
      type: "wire", event: { kind: "snapshot", snapshot: snapshot([completed], 4) },
    });
    const overtaken = agentReducer(initial, { type: "wire", event: {
      kind: "upsert",
      hostProfileId: "local",
      serverIdentity: "server-a",
      connectionEpoch: 1,
      sequence: agentGeneration(6),
      record: agent({ id: "other", lifecycleGeneration: 6 }),
    } });
    const acknowledged = agentReducer(overtaken, {
      type: "seenAck",
      agentId: completed.id,
      attentionGeneration: completed.attentionGeneration,
      attentionSeenAt: 5_000,
      hostProfileId: "local",
      serverIdentity: "server-a",
      connectionEpoch: 1,
    });
    expect(local(acknowledged).eventSequence).toBe(agentGeneration(6));
    expect(local(acknowledged).byId[completed.id]).toMatchObject({
      seenGeneration: agentGeneration(8),
      attentionSeenAt: 5_000,
    });
    expect(agentReducer(overtaken, {
      type: "seenAck",
      agentId: completed.id,
      attentionGeneration: completed.attentionGeneration,
      attentionSeenAt: 5_000,
      hostProfileId: "local",
      serverIdentity: "server-a",
      connectionEpoch: 2,
    })).toBe(overtaken);
  });

  it("rejects a snapshot whose accepted barrier is behind an already applied live event", () => {
    const initial = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()], 5) } });
    const renamed = agent({ displayName: "Live rename" });
    const live = agentReducer(initial, { type: "wire", event: { kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(7), record: renamed } });
    const lateSnapshot = { ...snapshot([agent({ displayName: "Stale name" })], 6), acceptedGeneration: agentGeneration(6), eventSequence: agentGeneration(6) };
    expect(agentReducer(live, { type: "wire", event: { kind: "snapshot", snapshot: lateSnapshot } })).toBe(live);
  });

  it("rejects a late mutation from a previous connection epoch", () => {
    const current = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()], 5) } });
    const staleRename = agent({ displayName: "Old connection" });
    const staleEvent = {
      kind: "upsert" as const,
      hostProfileId: "local",
      serverIdentity: "server-a",
      connectionEpoch: 0,
      sequence: agentGeneration(6),
      record: staleRename,
    };
    expect(agentReducer(current, { type: "wire", event: staleEvent })).toBe(current);
  });
});

describe("agentReducer across hosts", () => {
  const remoteAgent = (overrides: Parameters<typeof agent>[0] = {}) => agent({ hostProfileId: "remote", ...overrides });
  const both = agentReducer(
    agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } }),
    { type: "wire", event: { kind: "snapshot", snapshot: snapshot([remoteAgent()], 4, "remote") } },
  );

  it("keeps the same agent id on two hosts as two records", () => {
    expect(both.byHost.local.byId["agent-1"].hostProfileId).toBe("local");
    expect(both.byHost.remote.byId["agent-1"].hostProfileId).toBe("remote");
  });

  it("routes a live event to its own host and leaves the other slice untouched", () => {
    const renamed = agentReducer(both, { type: "wire", event: {
      kind: "upsert", hostProfileId: "remote", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5),
      record: remoteAgent({ displayName: "Renamed remotely" }),
    } });
    expect(renamed.byHost.local).toBe(both.byHost.local);
    expect(renamed.byHost.remote.byId["agent-1"].displayName).toBe("Renamed remotely");
    expect(both.byHost.local.byId["agent-1"].displayName).toBe("Codex one");
  });

  it("checks sequence, epoch and server identity against the event's own host", () => {
    // Host B is at sequence 4 while host A has moved on to 9. An event for B
    // at 5 is fresh for B even though A would have discarded it.
    const advanced = agentReducer(both, { type: "wire", event: {
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(9),
      record: agent({ displayName: "Far ahead" }),
    } });
    const remoteAt5 = agentReducer(advanced, { type: "wire", event: {
      kind: "retired", hostProfileId: "remote", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5),
      retiredAgentIds: ["agent-1"],
    } });
    expect(remoteAt5.byHost.remote.byId["agent-1"]).toBeUndefined();
    expect(remoteAt5.byHost.local.byId["agent-1"].displayName).toBe("Far ahead");
    // A's epoch and server identity are no alibi for an event addressed to B.
    const otherEpoch = agentReducer(both, { type: "wire", event: {
      kind: "retired", hostProfileId: "remote", serverIdentity: "server-a", connectionEpoch: 2, sequence: agentGeneration(5),
      retiredAgentIds: ["agent-1"],
    } });
    expect(otherEpoch).toBe(both);
    const otherServer = agentReducer(both, { type: "wire", event: {
      kind: "retired", hostProfileId: "remote", serverIdentity: "server-b", connectionEpoch: 1, sequence: agentGeneration(5),
      retiredAgentIds: ["agent-1"],
    } });
    expect(otherServer).toBe(both);
  });

  it("acknowledges seen only on the named host", () => {
    const unseen = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 3 })]) } });
    const twoHosts = agentReducer(unseen, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([remoteAgent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 3 })], 4, "remote") } });
    const acknowledged = agentReducer(twoHosts, {
      type: "seenAck", agentId: "agent-1", attentionGeneration: agentGeneration(8), attentionSeenAt: 5_000,
      hostProfileId: "remote", serverIdentity: "server-a", connectionEpoch: 1,
    });
    expect(acknowledged.byHost.remote.byId["agent-1"].seenGeneration).toBe(agentGeneration(8));
    expect(acknowledged.byHost.local).toBe(twoHosts.byHost.local);
    expect(acknowledged.byHost.local.byId["agent-1"].seenGeneration).toBe(agentGeneration(3));
  });

  it("disconnects one host without touching the other and removes a host outright", () => {
    const disconnected = agentReducer(both, { type: "disconnect", hostProfileId: "remote" });
    expect(disconnected.byHost.remote.authoritative).toBe(false);
    expect(disconnected.byHost.local).toBe(both.byHost.local);
    expect(agentReducer(both, { type: "disconnect", hostProfileId: "never-seen" })).toBe(both);
    const removed = agentReducer(both, { type: "remove", hostProfileId: "remote" });
    expect(removed.byHost.remote).toBeUndefined();
    expect(removed.byHost.local).toBe(both.byHost.local);
    expect(agentReducer(removed, { type: "remove", hostProfileId: "remote" })).toBe(removed);
  });

  it("resets every host", () => {
    expect(agentReducer(both, { type: "reset" })).toBe(initialAgentState);
  });
});
