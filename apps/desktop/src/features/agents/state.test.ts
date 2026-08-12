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

  it("does not let a newer weak heuristic overwrite unexpired hook authority", () => {
    const hook = agent({ authority: "hook", authorityExpiresAt: 500, updatedAt: 100 });
    const first = agentReducer(initialAgentState, { type: "wire", event: { kind: "snapshot", snapshot: snapshot([hook]) } });
    const screen = agent({ authority: "screen", lifecycle: "unknown", lifecycleGeneration: 4, updatedAt: 400 });
    const guarded = agentReducer(first, { type: "wire", event: { kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(5), record: screen } });
    expect(guarded.byId["agent-1"]).toMatchObject({ authority: "hook", lifecycle: "working" });
    const expired = agentReducer(guarded, { type: "wire", event: { kind: "upsert", hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, sequence: agentGeneration(6), record: { ...screen, updatedAt: 501 } } });
    expect(expired.byId["agent-1"].authority).toBe("screen");
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
