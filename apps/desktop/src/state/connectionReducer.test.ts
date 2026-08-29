import { describe, expect, it } from "vitest";
import { connectionReducer, initialHostState } from "./connectionReducer";

const empty = { sessions: [], windows: [], panes: [] };
const populated = {
  sessions: [{ id: "$1", name: "work", windowCount: 1, attachedClients: 0 }],
  windows: [{ id: "@2", sessionId: "$1", index: 0, name: "shell", active: true, layout: "" }],
  panes: [{
    id: "%3", sessionId: "$1", windowId: "@2", index: 0, active: true,
    width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "bash",
  }],
};

describe("connectionReducer", () => {
  it("replaces every authoritative entity on a snapshot and enables mutations only when connected", () => {
    const snapshotted = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 4, serverIdentity: "server-a",
    });
    expect(Object.keys(snapshotted.panes)).toEqual(["%3"]);
    expect(snapshotted.canMutate).toBe(false);
    const connected = connectionReducer(snapshotted, { type: "connection", phase: "connected" });
    expect(connected.canMutate).toBe(true);
  });

  it("rejects stale snapshots and applies a forward one without adjudicating the jump", () => {
    // Sequence integrity belongs to the native link, which repairs a break on
    // the connection it already holds. A snapshot that skips ahead is that
    // repair's own authoritative answer arriving — applying it is the point.
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, serverIdentity: "server-a",
    });
    const stale = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 7, serverIdentity: "server-a",
    });
    expect(stale).toBe(state);
    const jumped = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 9, serverIdentity: "server-a",
    });
    expect(jumped.phase).toBe(state.phase);
    expect(jumped.resyncRequested).toBe(false);
    expect(jumped.lastSequence).toBe(9);
    expect(jumped.panes).toEqual({});
  });

  it("keeps a forward event jump on the connection instead of freezing writes", () => {
    const state = connectionReducer({ ...initialHostState, phase: "connected", canMutate: true }, {
      type: "snapshot", snapshot: populated, sequence: 7, serverIdentity: "server-a",
    });
    const jumped = connectionReducer(state, { type: "orderedEvent", sequence: 10 });
    expect(jumped.lastSequence).toBe(10);
    expect(jumped.phase).toBe("connected");
    expect(jumped.canMutate).toBe(true);
    expect(jumped.resyncRequested).toBe(false);
    expect(connectionReducer(jumped, { type: "orderedEvent", sequence: 9 })).toBe(jumped);
  });

  it("advances sequence watermarks for non-snapshot terminal events", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, serverIdentity: "server-a",
    });
    const advanced = connectionReducer(state, { type: "orderedEvent", sequence: 8 });
    expect(advanced.lastSequence).toBe(8);
    const next = connectionReducer(advanced, {
      type: "orderedSnapshot", snapshot: populated, sequence: 9, generation: 2, serverIdentity: "server-a",
    });
    expect(next.lastSequence).toBe(9);
    expect(next.resyncRequested).toBe(false);
  });

  it("freezes on an event from a new tmux server until an authoritative snapshot arrives", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 12, serverIdentity: "old-server",
    });
    const mismatch = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 1, serverIdentity: "new-server",
    });
    expect(mismatch.serverIdentity).toBe("old-server");
    expect(mismatch.phase).toBe("resyncing");
    expect(mismatch.panes["%3"]).toBeDefined();
    const replaced = connectionReducer(mismatch, {
      type: "snapshot", snapshot: empty, sequence: 0, generation: 1, serverIdentity: "new-server",
    });
    expect(replaced.serverIdentity).toBe("new-server");
    expect(replaced.lastSequence).toBe(0);
    expect(replaced.panes).toEqual({});
  });

  it("names the one mismatch that requests a resync, and clears it on the next snapshot", () => {
    // A different tmux server is the only thing the frontend still rebuilds
    // for: sequence and generation ordering are the native link's to repair,
    // and rebuilding on them tore down the connection that repair had fixed.
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, generation: 5, serverIdentity: "server-a",
    });
    const identity = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 8, serverIdentity: "server-b",
    });
    expect(identity.resyncReason).toBe("snapshot-identity had=server-a received=server-b");
    const recovered = connectionReducer(identity, {
      type: "snapshot", snapshot: populated, sequence: 1, generation: 6, serverIdentity: "server-b",
    });
    expect(recovered.resyncReason).toBeUndefined();
    expect(recovered.resyncRequested).toBe(false);
  });

  it("ignores a snapshot whose generation went backwards, and re-baselines after a reconnect", () => {
    const live = connectionReducer(
      connectionReducer(initialHostState, { type: "connection", phase: "connected" }),
      { type: "snapshot", snapshot: populated, sequence: 7, generation: 12, serverIdentity: "server-a" },
    );
    // A frame overtaken in flight. Taking it would pin every later action to a
    // generation the host has already left, and each one would be refused as
    // stale until the next snapshot happened to arrive.
    const late = connectionReducer(live, {
      type: "snapshot", snapshot: empty, sequence: 8, generation: 11, serverIdentity: "server-a",
    });
    expect(late).toBe(live);
    // Both restarts still land: another tmux server, and a reconnected link to
    // the same one whose host process counts from zero again.
    const otherServer = connectionReducer(live, {
      type: "snapshot", snapshot: empty, sequence: 1, generation: 1, serverIdentity: "server-b",
    });
    expect(otherServer.generation).toBe(1);
    const reconnected = connectionReducer(
      connectionReducer(live, { type: "connection", phase: "connected" }),
      { type: "snapshot", snapshot: empty, sequence: 0, generation: 1, serverIdentity: "server-a" },
    );
    expect(reconnected.generation).toBe(1);
    expect(reconnected.panes).toEqual({});
  });

  it("keeps the world a reconciliation acknowledgement did not describe", () => {
    // The host answered a notification burst by finding nothing moved. It
    // carries the generation and no tree — resending the server to say
    // "unchanged" is tens of kilobytes ahead of the switch that burst belongs
    // to — so the entities, the identity and the generation all stand, and
    // only the sequence watermark advances.
    const live = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, generation: 4, serverIdentity: "server-a",
    });
    const acknowledged = connectionReducer(live, {
      type: "snapshot", sequence: 8, generation: 4, serverIdentity: "server-a",
    });
    expect(acknowledged.panes).toEqual(live.panes);
    expect(acknowledged.windows).toEqual(live.windows);
    expect(acknowledged.sessions).toEqual(live.sessions);
    expect(acknowledged.generation).toBe(4);
    expect(acknowledged.serverIdentity).toBe("server-a");
    expect(acknowledged.lastSequence).toBe(8);

    // And it never answers a rebuild this process asked for: only a world can.
    const resyncing = connectionReducer(live, {
      type: "orderedSnapshot", snapshot: empty, sequence: 9, serverIdentity: "server-b",
    });
    expect(resyncing.resyncRequested).toBe(true);
    const stillResyncing = connectionReducer(resyncing, {
      type: "snapshot", sequence: 10, generation: 4, serverIdentity: "server-a",
    });
    expect(stillResyncing.resyncRequested).toBe(true);
  });

  it("accepts a lower sequence authoritative reconnect snapshot for the same server", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 42, serverIdentity: "same-server",
    });
    const reconciled = connectionReducer(state, {
      type: "snapshot", snapshot: empty, sequence: 0, serverIdentity: "same-server",
    });
    expect(reconciled.lastSequence).toBe(0);
    expect(reconciled.panes).toEqual({});
    expect(reconciled.resyncRequested).toBe(false);
  });
});
