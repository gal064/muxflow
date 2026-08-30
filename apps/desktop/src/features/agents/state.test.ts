import { describe, expect, it } from "vitest";
import { acknowledgeSeen, agentReducer, initialAgentState } from "./state";
import type { AgentRecord, AgentSnapshot } from "./types";
import { agent } from "./testFixtures";
import { agentGeneration } from "./generation";
import { deriveAgentRollups } from "./selectors";

const snapshot = (agents: AgentRecord[], revision = 4): AgentSnapshot => ({
  hostProfileId: "local", serverIdentity: "server-a", revision: agentGeneration(revision), eventSequence: agentGeneration(revision),
  connectionEpoch: 1,
  acceptedGeneration: agentGeneration(revision),
  notificationWatermark: agentGeneration(revision), authoritative: true, agents, adapters: [],
});

describe("agentReducer", () => {
  it("replaces reconnect state with an authoritative snapshot without merging stale agents", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const disconnected = agentReducer(first, { type: "disconnect" });
    expect(disconnected.authoritative).toBe(false);
    const replaced = agentReducer(disconnected, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([], 5), replayed: true } });
    expect(replaced.authoritative).toBe(true);
    expect(replaced.byId).toEqual({});
  });

  it("rejects stale, cross-server, and weaker lifecycle generations", () => {
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent()]) } });
    const weaker = agent({ lifecycle: "unknown", lifecycleGeneration: 2, updatedAt: 120 });
    const state = agentReducer(first, { type: "wire", event: { kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5), record: weaker } });
    expect(state.byId["agent-1"].lifecycle).toBe("working");
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
    expect(retired.byId["agent-1"]).toBeUndefined();
    expect(retired.eventSequence).toEqual(agentGeneration(5));
  });

  it("ignores a retirement from another host and one for an agent it never had", () => {
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
    expect(unknown.byId["agent-1"]).toBeDefined();
    expect(unknown.eventSequence).toEqual(agentGeneration(5));
  });

  it("atomically retires a manually detected identity when the native identity arrives", () => {
    const manual = agent({ id: "manual", nativeSessionId: "", detectedManually: true });
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([manual]) } });
    const native = agent({ id: "native", nativeSessionId: "session-1", lifecycleGeneration: 5 });
    const promoted = agentReducer(first, { type: "wire", event: {
      kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5),
      record: native, retiredAgentIds: ["manual"],
    } });
    expect(Object.keys(promoted.byId)).toEqual(["native"]);
  });

  it("retains an unmapped record without inventing topology", () => {
    const unmapped = agent({ sessionId: "", windowId: "", paneId: "" });
    const state = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([unmapped]) } });
    expect(state.byId[unmapped.id]).toMatchObject({ paneId: "", sessionId: "", windowId: "" });
    const rollups = deriveAgentRollups(Object.values(state.byId));
    expect(rollups.byPane.size).toBe(0);
    expect(rollups.byWindow.size).toBe(0);
    expect(rollups.byWorkspace.size).toBe(0);
  });

  it("acknowledges only the exact current attention generation", () => {
    const current = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([agent({ lifecycle: "idle", attentionGeneration: 8, seenGeneration: 3 })]) } });
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
    expect(acknowledged.eventSequence).toBe(agentGeneration(6));
    expect(acknowledged.byId[completed.id]).toMatchObject({
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
