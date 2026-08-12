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

  it("rejects stale events and freezes writes on a sequence gap", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, serverIdentity: "server-a",
    });
    const stale = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 7, serverIdentity: "server-a",
    });
    expect(stale).toBe(state);
    const gap = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 9, serverIdentity: "server-a",
    });
    expect(gap.phase).toBe("resyncing");
    expect(gap.canMutate).toBe(false);
    expect(gap.resyncRequested).toBe(true);
    expect(Object.keys(gap.panes)).toEqual(["%3"]);
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

  it("freezes atomically on an already-detected payload gap without advancing or mutating entities", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 7, serverIdentity: "server-a",
    });
    const gap = connectionReducer(state, { type: "sequenceGap", expected: 8, received: 9 });
    expect(gap).toMatchObject({ lastSequence: 7, phase: "resyncing", canMutate: false, resyncRequested: true });
    expect(gap.panes).toEqual(state.panes);
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

  it("rejects an ordered snapshot with a stale host generation and freezes writes", () => {
    const state = connectionReducer(initialHostState, {
      type: "snapshot", snapshot: populated, sequence: 2, generation: 5, serverIdentity: "server-a",
    });
    const stale = connectionReducer(state, {
      type: "orderedSnapshot", snapshot: empty, sequence: 3, generation: 4, serverIdentity: "server-a",
    });
    expect(stale.phase).toBe("resyncing");
    expect(stale.canMutate).toBe(false);
    expect(stale.panes["%3"]).toBeDefined();
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
