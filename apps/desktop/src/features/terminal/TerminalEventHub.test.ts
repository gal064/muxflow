import { describe, expect, it } from "vitest";
import { TerminalEventHub } from "./TerminalEventHub";
import type { TerminalEvent } from "./api";
import { copyTerminalBytes } from "./TerminalBytes";

const output = (sequence: number, generation: number, paneId = "%1", data = Uint8Array.of(generation)): TerminalEvent => ({
  kind: "output", paneId, generation, data: copyTerminalBytes(data), sequence,
});
const seed = (sequence: number, generation: number, paneId = "%1", data = Uint8Array.of(generation)): TerminalEvent => ({
  kind: "seed", paneId, generation, data: copyTerminalBytes(data), sequence,
});
type Resource = Extract<TerminalEvent, { kind: "paneResource" }>;
type ResourceOverrides = Partial<Omit<Resource, "serializedSnapshot" | "rawTail">> & {
  serializedSnapshot?: Uint8Array;
  rawTail?: Uint8Array;
};
const resource = (
  sequence: number,
  generation: number,
  overrides: ResourceOverrides = {},
): TerminalEvent => {
  const { serializedSnapshot = Uint8Array.of(generation), rawTail = Uint8Array.of(generation + 10), ...metadata } = overrides;
  return {
    kind: "paneResource", paneId: "%1", state: "hiddenBuffered", requiresSeed: false,
    recoveryReason: "", generation, snapshotGeneration: Math.max(0, generation - 1), tailThroughGeneration: generation,
    serializedSnapshot: copyTerminalBytes(serializedSnapshot), rawTail: copyTerminalBytes(rawTail), sequence, ...metadata,
  };
};

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

  it("asks for a usable seed instead of silently dropping one that looks stale", () => {
    // A seed is a whole screen, not an increment. Dropping one because its
    // generation is behind the hub's watermark leaves the pane waiting for
    // content that has already been sent and will never be sent again
    // (P12-U003.3), so the drop has to turn into an explicit request.
    const requested: string[] = [];
    const hub = new TerminalEventHub((paneId) => requested.push(paneId));
    hub.publish(output(1, 7));
    hub.publish(seed(2, 3));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([output(1, 7)]);
    expect(requested).toEqual(["%1"]);
    // The request is made once, not once per stale seed.
    hub.publish(seed(3, 4));
    expect(requested).toEqual(["%1"]);
    // And a seed the hub can accept clears the debt.
    hub.publish(seed(4, 9));
    hub.publish(seed(5, 5));
    expect(requested).toEqual(["%1", "%1"]);
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

  it("enforces one ownership consumer for each pane", () => {
    const hub = new TerminalEventHub();
    const unsubscribe = hub.subscribePane("%1", () => undefined);
    expect(() => hub.subscribePane("%1", () => undefined)).toThrow(
      "terminal pane %1 already has an active consumer",
    );
    unsubscribe();
    expect(() => hub.subscribePane("%1", () => undefined)).not.toThrow();
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

  it("delivers dedicated epoch subscriptions only for admitted epoch frames", () => {
    const hub = new TerminalEventHub();
    const epochs: number[] = [];
    let appDeliveries = 0;
    const unsubscribe = hub.subscribeEpoch((event) => epochs.push(event.epoch));
    hub.publish(output(1, 1));
    hub.publish({ kind: "generationEpoch", epoch: 41, sequence: 0 }, () => { appDeliveries += 1; });
    hub.publish(output(1, 1));
    hub.publish({ kind: "generationEpoch", epoch: 41, sequence: 0 }, () => { appDeliveries += 1; });
    expect(hub.publish(output(2, 2))).toEqual({ kind: "accepted" });
    hub.publish({ kind: "connectionState", state: "connected", sequence: 0 });
    expect(epochs).toEqual([41]);
    expect(appDeliveries).toBe(1);
    unsubscribe();
    hub.publish({ kind: "generationEpoch", epoch: 42, sequence: 0 });
    expect(epochs).toEqual([41]);
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

  it("bounds zero-byte hidden records as well as retained bytes", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxPaneEvents: 2 },
    );
    hub.publish(output(1, 1, "%7", new Uint8Array()));
    hub.publish(output(2, 2, "%7", new Uint8Array()));
    hub.publish(output(3, 3, "%7", new Uint8Array()));
    expect(requests).toEqual(["%7"]);
    expect(hub.retainedByteLength).toBe(0);
    expect(hub.retainedPaneCount).toBe(0);
  });

  it("bounds a hidden resource's recovery reason even when its byte segments are empty", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId), { maxPaneBytes: 4 });
    hub.publish(resource(1, 1, {
      recoveryReason: "12345", serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array(),
    }));
    expect(requests).toEqual(["%1"]);
    expect(hub.retainedByteLength).toBe(0);
  });

  it("accounts the exact backing allocations retained for hidden output", () => {
    const hub = new TerminalEventHub();
    hub.publish(output(1, 1, "%7", Uint8Array.of(1, 2, 3)));
    hub.publish(output(2, 2, "%7", Uint8Array.of(4, 5, 6, 7)));
    expect(hub.retainedByteLength).toBe(7);
    const received: Array<Extract<TerminalEvent, { kind: "output" }>> = [];
    hub.subscribePane("%7", (event) => { if (event.kind === "output") received.push(event); });
    expect(received.reduce((total, event) => total + event.data.buffer.byteLength, 0)).toBe(7);
  });

  it("replaces a consumed hidden recovery checkpoint instead of replaying its stale raw tail", () => {
    const hub = new TerminalEventHub();
    hub.publish(resource(1, 1));
    hub.publish(resource(2, 2));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([resource(2, 2)]);
  });

  it("keeps exact byte accounting while replacing resource and diagnostic boundaries", () => {
    const hub = new TerminalEventHub();
    hub.publish(output(1, 1, "%1", Uint8Array.of(1, 2, 3)));
    hub.publish({ kind: "seedDiagnostic", paneId: "%1", message: "old", sequence: 2 });
    hub.publish(resource(3, 2, { serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array() }));
    hub.publish({ kind: "seedDiagnostic", paneId: "%1", message: "λ", sequence: 4 });
    expect(hub.retainedByteLength).toBe(5);
    hub.publish(resource(5, 3, { recoveryReason: "λ", serializedSnapshot: Uint8Array.of(7), rawTail: Uint8Array.of(8, 9) }));
    expect(hub.retainedByteLength).toBe(5);
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([
      resource(5, 3, { recoveryReason: "λ", serializedSnapshot: Uint8Array.of(7), rawTail: Uint8Array.of(8, 9) }),
    ]);
    expect(hub.retainedByteLength).toBe(0);
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
    const topology = { kind: "snapshot", snapshot: { sessions: [], windows: [], panes: [] }, generation: 7, serverIdentity: "server", authoritative: true, sequence: 12 } as TerminalEvent;
    const agents = { kind: "agentService", scope: "snapshot", snapshot: { generation: "7", acceptedGeneration: "7", agents: [], authoritative: true, notificationWatermark: "7", connectionEpoch: "41" }, sequence: 0 } as TerminalEvent;
    expect(hub.publish(topology, () => received.push(topology))).toEqual({ kind: "accepted" });
    expect(hub.publish(agents, () => received.push(agents))).toEqual({ kind: "local" });
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

  it("uses conservative reseed after transferring a resource to an active pane", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.subscribePane("%1", () => undefined);
    const checkpoint = resource(1, 2);
    hub.publish(checkpoint);
    expect(hub.retainedByteLength).toBe(0);
    if (checkpoint.kind !== "paneResource") throw new Error("expected resource fixture");
    checkpoint.rawTail[0] = 99;
    hub.publish({ ...checkpoint, sequence: 2 });
    expect(requests).toEqual(["%1"]);
  });

  it("retains one exclusively owned dormant resource payload", () => {
    const hub = new TerminalEventHub();
    hub.publish(resource(1, 2));
    // The replay event is also the exact duplicate-comparison identity: no
    // detached second allocation is retained beside it.
    expect(hub.retainedByteLength).toBe(2);
    expect(hub.retainedPaneCount).toBe(1);
    hub.subscribePane("%1", () => undefined);
    expect(hub.retainedByteLength).toBe(0);
    expect(hub.retainedPaneCount).toBe(0);
  });

  it("does not retain a detached active resource when its renderer throws", () => {
    const hub = new TerminalEventHub();
    hub.subscribePane("%1", () => { throw new Error("injected renderer failure"); });
    expect(() => hub.publish(resource(1, 2))).not.toThrow();
    expect(hub.retainedByteLength).toBe(0);
    expect(hub.retainedPaneCount).toBe(0);
  });

  it("requires a seed after a mounted consumer rejects an admitted event", () => {
    const requests: string[] = [];
    const received: TerminalEvent[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.subscribePane("%1", (event) => {
      received.push(event);
      if (event.kind === "output") throw new Error("renderer rejected output");
    });
    expect(() => hub.publish(output(1, 1))).not.toThrow();
    expect(hub.publish(output(2, 2))).toEqual({ kind: "accepted" });
    expect(received).toEqual([output(1, 1)]);
    expect(requests).toEqual(["%1"]);
  });

  it("rolls back subscription ownership and requires a seed when backlog replay throws", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.publish(output(1, 1));
    hub.publish(output(2, 2));
    expect(() => hub.subscribePane("%1", () => {
      throw new Error("renderer unavailable");
    })).toThrow("renderer unavailable");

    const received: TerminalEvent[] = [];
    expect(() => hub.subscribePane("%1", (event) => received.push(event))).not.toThrow();
    hub.publish(output(3, 3));
    expect(received).toEqual([]);
    hub.publish(seed(4, 4));
    expect(received).toEqual([seed(4, 4)]);
    expect(requests).toEqual(["%1"]);
  });

  it("conservatively reseeds an oversized same-generation checkpoint", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.subscribePane("%1", () => undefined);
    const checkpoint = resource(1, 2, {
      rawTail: new Uint8Array(256 * 1024 + 1),
      serializedSnapshot: new Uint8Array(),
    });
    hub.publish(checkpoint);
    hub.publish({ ...checkpoint, sequence: 2 });
    expect(requests).toEqual(["%1"]);
  });

  it("releases consumed resource allocations and reseeds conservatively", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    const checkpoint = resource(1, 2);
    hub.publish(checkpoint);
    const consumed: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => consumed.push(event));
    expect(hub.retainedByteLength).toBe(0);
    const delivered = consumed[0];
    if (delivered.kind !== "paneResource") throw new Error("expected resource fixture");
    delivered.serializedSnapshot[0] = 99;
    hub.publish({ ...delivered, sequence: 2 });
    expect(requests).toEqual(["%1"]);
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

  it("preserves seed debt when a hidden backlog is evicted from the metadata LRU", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxTrackedPanes: 1, maxBufferedPanes: 2 },
    );
    hub.publish(output(1, 1, "%1"));
    hub.publish(output(2, 1, "%2"));
    expect(requests).toEqual(["%1"]);

    // Re-entry before the requested seed must not start a truncated backlog.
    hub.publish(output(3, 2, "%1"));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([]);
    expect(requests).toEqual(["%1", "%2"]);

    hub.publish(seed(4, 3, "%1"));
    hub.publish(output(5, 4, "%1"));
    expect(received).toEqual([seed(4, 3, "%1"), output(5, 4, "%1")]);
  });

  it("pins an active pane's generation, resource identity, and rendered checkpoint", () => {
    const requests: string[] = [];
    const received: TerminalEvent[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxTrackedPanes: 1, maxBufferedPanes: 2 },
    );
    hub.publish({ kind: "generationEpoch", epoch: 7, sequence: 0 });
    hub.subscribePane("%1", (event) => received.push(event));
    hub.publish(seed(1, 5, "%1"));
    hub.markRendered("%1", 5, 7);

    // Churn dormant panes past their independent one-slot metadata limit. The
    // subscribed pane remains authoritative without participating in the LRU.
    hub.publish(output(2, 1, "%2"));
    hub.publish(output(3, 1, "%3"));
    hub.publish(output(4, 4, "%1"));

    expect(received).toEqual([seed(1, 5, "%1")]);
    expect(hub.visibilityCheckpoint("%1")).toEqual({ terminalEpoch: 7, outputGeneration: 5 });
    expect(requests).toEqual(["%2"]);
  });

  it("keeps mounted panes outside the dormant LRU capacity and hot path", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxTrackedPanes: 1, maxBufferedPanes: 1 },
    );
    hub.publish({ kind: "generationEpoch", epoch: 9, sequence: 0 });
    const received = new Map<string, number>();
    for (let index = 1; index <= 32; index += 1) {
      const paneId = `%${index}`;
      hub.subscribePane(paneId, () => received.set(paneId, (received.get(paneId) ?? 0) + 1));
      hub.publish(seed(index, index, paneId));
      hub.markRendered(paneId, index, 9);
    }

    expect(requests).toEqual([]);
    expect(received.size).toBe(32);
    expect(hub.trackedPaneCount).toBe(32);
    expect(hub.visibilityCheckpoint("%1")).toEqual({ terminalEpoch: 9, outputGeneration: 1 });
    expect(hub.visibilityCheckpoint("%32")).toEqual({ terminalEpoch: 9, outputGeneration: 32 });
  });

  it("requires a seed conservatively after the bounded debt tombstone ages out", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxTrackedPanes: 1, maxBufferedPanes: 3 },
    );
    hub.publish(output(1, 1, "%1"));
    hub.publish(output(2, 1, "%2"));
    hub.publish(output(3, 1, "%3"));
    // The one-entry tombstone can no longer name %1, but the bounded fallback
    // still refuses incremental output when that pane eventually re-enters.
    hub.publish(output(4, 2, "%1"));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([]);
    expect(requests).toEqual(["%1", "%2", "%3", "%1"]);
  });

  it("clamps a zero metadata capacity before it can truncate a pane backlog", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId), { maxTrackedPanes: 0 });
    hub.publish(output(1, 1, "%1"));
    hub.publish(output(2, 2, "%1"));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    expect(received).toEqual([output(1, 1, "%1"), output(2, 2, "%1")]);
    expect(requests).toEqual([]);
    expect(hub.trackedPaneCount).toBe(1);
  });

  it.each([
    ["seed", seed(4, 3, "%1")],
    ["recovery material", resource(4, 3, { paneId: "%1" })],
    ["host-owned seed request", resource(4, 3, {
      paneId: "%1", requiresSeed: true, recoveryReason: "host recovery pending",
      serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array(),
    })],
    ["diagnostic preceding a seed", { kind: "seedDiagnostic", paneId: "%1", message: "partial metadata", sequence: 4 } as TerminalEvent],
  ])("does not issue a redundant conservative request for an untracked %s", (_name, repairEvent) => {
    const requests: string[] = [];
    const hub = new TerminalEventHub(
      (paneId) => requests.push(paneId),
      { maxTrackedPanes: 1, maxBufferedPanes: 3 },
    );
    hub.publish(output(1, 1, "%1"));
    hub.publish(output(2, 1, "%2"));
    hub.publish(output(3, 1, "%3"));
    hub.publish(repairEvent);
    // %1's original request remains authoritative; touching %1 evicts %3,
    // but the repairing/host-owned event must not request %1 a second time.
    expect(requests.filter((paneId) => paneId === "%1")).toEqual(["%1"]);
  });
});
