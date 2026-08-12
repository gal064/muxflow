import { describe, expect, it } from "vitest";
import { TerminalEventHub } from "./TerminalEventHub";
import type { TerminalEvent } from "./api";

const output = (sequence: number, generation: number, paneId = "%1", data = Uint8Array.of(generation)): TerminalEvent => ({
  kind: "output", paneId, generation, data, sequence,
});
const seed = (sequence: number, generation: number, paneId = "%1", data = Uint8Array.of(generation)): TerminalEvent => ({
  kind: "seed", paneId, generation, data, sequence,
});
const resource = (
  sequence: number,
  generation: number,
  overrides: Partial<Extract<TerminalEvent, { kind: "paneResource" }>> = {},
): TerminalEvent => ({
  kind: "paneResource", paneId: "%1", state: "hiddenBuffered", requiresSeed: false,
  recoveryReason: "", generation, snapshotGeneration: Math.max(0, generation - 1), tailThroughGeneration: generation,
  serializedSnapshot: Uint8Array.of(generation),
  rawTail: Uint8Array.of(generation + 10), sequence, ...overrides,
});

describe("TerminalEventHub hidden-pane buffering", () => {
  it("replays byte-exact hidden output when a pane becomes visible", () => {
    const hub = new TerminalEventHub();
    hub.publish(output(1, 1, "%1", Uint8Array.from([0, 255, 27])));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([output(1, 1, "%1", Uint8Array.from([0, 255, 27]))]);
  });

  it("replaces stale hidden output when a fresh seed arrives", () => {
    const hub = new TerminalEventHub();
    hub.publish(output(1, 1));
    hub.publish(seed(2, 2));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([seed(2, 2)]);
  });

  it("does not replay output already delivered to a mounted pane", () => {
    const hub = new TerminalEventHub();
    const unsubscribe = hub.subscribePane("%1", () => undefined);
    hub.publish(output(1, 1));
    unsubscribe();
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([]);
  });

  it("drops hidden state and old generation watermarks at a new terminal epoch", () => {
    const hub = new TerminalEventHub();
    hub.publish({ kind: "generationEpoch", epoch: 1, sequence: 0 });
    hub.publish(output(1, 9_000));
    hub.publish({ kind: "generationEpoch", epoch: 2, sequence: 0 });
    hub.publish(seed(1, 1));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([seed(1, 1)]);
  });

  it("ignores duplicate terminal generations and waits for seed after recovery invalidation", () => {
    const hub = new TerminalEventHub();
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    hub.publish(output(1, 5));
    hub.publish(output(2, 5, "%1", Uint8Array.of(9)));
    hub.publish(resource(3, 6, {
      state: "released", requiresSeed: true, recoveryReason: "overflow",
      serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array(),
    }));
    hub.publish(output(4, 7));
    hub.publish(seed(5, 8));
    hub.publish(output(6, 9));
    expect(received.map((event) => event.kind)).toEqual(["output", "paneResource", "seed", "output"]);
  });

  it("requests one scoped seed on a bounded hidden backlog overflow and suppresses until that seed", () => {
    const requests: Array<{ paneId: string; reason: string }> = [];
    const hub = new TerminalEventHub(
      (paneId, reason) => requests.push({ paneId, reason }),
      { maxPaneBytes: 8, maxTotalBytes: 16 },
    );
    hub.publish(output(1, 1, "%7", new Uint8Array(8)));
    hub.publish(output(2, 2, "%7", Uint8Array.of(2)));
    hub.publish(output(3, 3, "%7", Uint8Array.of(3)));
    expect(requests).toEqual([{ paneId: "%7", reason: "frontend hidden recovery buffer exceeded 8 bytes" }]);
    hub.publish(seed(4, 4, "%7", Uint8Array.of(4)));
    hub.publish(output(5, 5, "%7", Uint8Array.of(5)));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%7", (event) => received.push(event));
    expect(received).toEqual([seed(4, 4, "%7", Uint8Array.of(4)), output(5, 5, "%7", Uint8Array.of(5))]);
  });

  it("replaces a consumed hidden recovery checkpoint instead of replaying its stale raw tail", () => {
    const hub = new TerminalEventHub();
    hub.publish(resource(1, 1));
    hub.publish(resource(2, 2));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([resource(2, 2)]);
  });

  it("routes seed diagnostics only to their pane without requesting recovery", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    const paneOne: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => paneOne.push(event));
    hub.publish({ kind: "seedDiagnostic", paneId: "%1", message: "partial alternate metadata", sequence: 1 });
    expect(paneOne).toEqual([{ kind: "seedDiagnostic", paneId: "%1", message: "partial alternate metadata", sequence: 1 }]);
    expect(requests).toEqual([]);
  });

  it("rejects a global sequence gap before delivering its pane payload and stays frozen", () => {
    const hub = new TerminalEventHub();
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(hub.publish(output(1, 1))).toEqual({ kind: "accepted" });
    expect(hub.publish(output(3, 2))).toEqual({ kind: "gap", expected: 2, received: 3 });
    expect(hub.publish(output(2, 3))).toEqual({ kind: "gap", expected: 2, received: 2 });
    expect(received).toEqual([output(1, 1)]);
    expect(hub.lastSequence).toBe(1);
  });

  it("admits a sequence-zero agent snapshot paired with an authoritative topology snapshot", () => {
    const hub = new TerminalEventHub();
    const received: TerminalEvent[] = [];
    hub.subscribe((event) => received.push(event));
    const topology = { kind: "snapshot", snapshot: { sessions: [], windows: [], panes: [] }, generation: 7, serverIdentity: "server", authoritative: true, sequence: 12 } as TerminalEvent;
    const agents = { kind: "agentService", scope: "snapshot", snapshot: { generation: "7", acceptedGeneration: "7", agents: [], authoritative: true, notificationWatermark: "7", connectionEpoch: "41" }, sequence: 0 } as TerminalEvent;
    expect(hub.publish(topology)).toEqual({ kind: "accepted" });
    expect(hub.publish(agents)).toEqual({ kind: "local" });
    expect(received).toEqual([topology, agents]);
    expect(hub.lastSequence).toBe(12);
  });

  it("treats repeated checkpoints as idempotent but scoped-reseeds a conflicting handoff without discarding backlog", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.publish(output(1, 1));
    const checkpoint = resource(2, 2);
    hub.publish(checkpoint);
    hub.publish(resource(3, 2));
    expect(requests).toEqual([]);
    hub.publish(resource(4, 2, { rawTail: Uint8Array.of(99) }));
    expect(requests).toEqual(["%1"]);
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([checkpoint]);
  });

  it("keeps output arriving after hide serialization outside the exact cutoff and available for recovery", () => {
    const hub = new TerminalEventHub();
    hub.publish({ kind: "generationEpoch", epoch: 42, sequence: 0 });
    const unsubscribe = hub.subscribePane("%1", () => undefined);
    hub.publish(output(1, 7));
    hub.markRendered("%1", 7);
    const serializedCheckpoint = hub.visibilityCheckpoint("%1");
    unsubscribe();
    hub.publish(output(2, 8));
    expect(serializedCheckpoint).toEqual({ terminalEpoch: 42, outputGeneration: 7 });
    expect(hub.visibilityCheckpoint("%1")?.outputGeneration).toBe(7);
    const recovered: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => recovered.push(event));
    expect(recovered).toEqual([output(2, 8)]);
  });

  it("invalidates rendered cutoffs across a terminal epoch mismatch", () => {
    const hub = new TerminalEventHub();
    hub.publish({ kind: "generationEpoch", epoch: 41, sequence: 0 });
    hub.markRendered("%1", 99);
    expect(hub.visibilityCheckpoint("%1")?.outputGeneration).toBe(99);
    hub.publish({ kind: "generationEpoch", epoch: 42, sequence: 0 });
    expect(hub.visibilityCheckpoint("%1")).toEqual({ terminalEpoch: 42, outputGeneration: 0 });
  });

  it("bounds aggregate hidden bytes and pane metadata across many panes", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxPaneBytes: 16, maxTotalBytes: 24, maxBufferedPanes: 3, maxTrackedPanes: 5 },
    );
    for (let pane = 1; pane <= 20; pane += 1) {
      hub.publish(seed(pane, 1, `%${pane}`, new Uint8Array(8)));
    }
    expect(hub.retainedPaneCount).toBeLessThanOrEqual(3);
    expect(hub.retainedByteLength).toBeLessThanOrEqual(24);
    expect(hub.trackedPaneCount).toBeLessThanOrEqual(5);
    expect(requests.length).toBeGreaterThan(0);
    const received: TerminalEvent[] = [];
    hub.subscribePane("%20", (event) => received.push(event));
    expect(received).toEqual([seed(20, 1, "%20", new Uint8Array(8))]);
  });
});
