import { beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalEventHub } from "./TerminalEventHub";
import type { TerminalEvent } from "./api";
import { copyTerminalBytes } from "./TerminalBytes";
import { recordIncident } from "../../diagnostics/incidents";

vi.mock("../../diagnostics/incidents", () => ({ recordIncident: vi.fn() }));
const incidents = vi.mocked(recordIncident);
beforeEach(() => incidents.mockClear());

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

/**
 * The host's answer to a reveal it could verify: no screen, the output since
 * the checkpoint, and the flag that says so. `resumeFromRenderer` lands with
 * the proto change in step 3 (§2).
 */
const resumeAnswer = (sequence: number, generation: number): TerminalEvent => {
  const answer: Resource & { resumeFromRenderer: boolean } = {
    ...(resource(sequence, generation, {
      serializedSnapshot: new Uint8Array(),
      rawTail: new Uint8Array(),
    }) as Resource),
    resumeFromRenderer: true,
  };
  return answer;
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

  it("contains app and epoch observers after ownership transfer and admits the next frame", () => {
    const failures: string[] = [];
    const hub = new TerminalEventHub(undefined, {}, undefined, (message) => failures.push(message));
    hub.subscribeEpoch(() => { throw new Error("epoch observer rejected delivery"); });
    expect(() => hub.publish(
      { kind: "generationEpoch", epoch: 41, sequence: 0 },
      () => { throw new Error("app observer rejected delivery"); },
    )).not.toThrow();
    expect(hub.publish(output(1, 1))).toEqual({ kind: "accepted" });
    expect(failures).toHaveLength(2);
    expect(failures[0]).toContain("application delivery observer failed after terminal event ownership transfer");
    expect(failures[1]).toContain("terminal epoch observer failed after terminal event ownership transfer");
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

  it("admits a forward sequence jump, carries the watermark, and reseeds the mounted panes once", () => {
    // The native link owns sequence integrity and repairs a break in place. A
    // hole reaching this far is a frame this process dropped, so the answer is
    // scoped: deliver what arrived and re-establish the panes' screens — never
    // freeze the connection the native side has already made whole.
    const requests: [string, string][] = [];
    const hub = new TerminalEventHub((paneId, reason) => requests.push([paneId, reason]));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event));
    hub.subscribePane("%2", () => undefined);
    expect(hub.publish(output(1, 1))).toEqual({ kind: "accepted" });
    expect(requests).toEqual([]);
    expect(incidents).not.toHaveBeenCalled();

    // A burst of three jumps is one anomaly: one reseed per pane, not three.
    expect(hub.publish(output(4, 2))).toEqual({ kind: "accepted" });
    expect(hub.publish(output(9, 3))).toEqual({ kind: "accepted" });
    expect(hub.publish(output(20, 4))).toEqual({ kind: "accepted" });
    expect(hub.lastSequence).toBe(20);
    expect(received).toEqual([output(1, 1), output(4, 2), output(9, 3), output(20, 4)]);
    expect(requests).toEqual([
      ["%1", "event sequence jumped (decode drop?)"],
      ["%2", "event sequence jumped (decode drop?)"],
    ]);
    expect(incidents.mock.calls).toEqual([["link.eventGap", { expected: 2, received: 4 }]]);

    // Contiguous traffic after the jump costs nothing at all.
    expect(hub.publish(output(21, 5))).toEqual({ kind: "accepted" });
    expect(requests).toHaveLength(2);
    expect(incidents).toHaveBeenCalledTimes(1);
    // And a frame from behind the watermark is still dropped before delivery.
    expect(hub.publish(output(20, 6))).toEqual({ kind: "stale" });
    expect(received).toHaveLength(5);
  });

  it("fast-forwards to an authoritative snapshot without calling a jump a jump", () => {
    // The resync barrier arrives at whatever sequence the host reached. It is
    // the repair, not evidence of a loss, so it must not cost a pane reseed.
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    hub.subscribePane("%1", () => undefined);
    hub.publish(output(1, 1));
    const barrier = {
      kind: "snapshot", snapshot: { sessions: [], windows: [], panes: [] },
      generation: 3, serverIdentity: "server", authoritative: true, sequence: 77,
    } as TerminalEvent;
    expect(hub.publish(barrier)).toEqual({ kind: "accepted" });
    expect(hub.lastSequence).toBe(77);
    expect(hub.publish(output(78, 2))).toEqual({ kind: "accepted" });
    expect(requests).toEqual([]);
    expect(incidents).not.toHaveBeenCalled();
  });

  it("reseeds every mounted pane on request, reopening a one-shot conflict latch", () => {
    const requests: [string, string][] = [];
    const hub = new TerminalEventHub((paneId, reason) => requests.push([paneId, reason]));
    const unsubscribe = hub.subscribePane("%1", () => undefined);
    hub.subscribePane("%2", () => undefined);
    // %1 arrives already holding the one-shot conflict latch.
    hub.publish(output(1, 7));
    hub.publish(seed(2, 3));
    expect(requests).toEqual([["%1", "stale or conflicting terminal visibility handoff"]]);

    hub.reseedSubscribedPanes("post-resync reseed");
    expect(requests.slice(1)).toEqual([["%1", "post-resync reseed"], ["%2", "post-resync reseed"]]);
    expect(hub.paneHealth("%1").conflictReseedRequested).toBe(false);

    // Unmounted panes are not asked for: nothing is rendering them.
    unsubscribe();
    hub.reseedSubscribedPanes("post-resync reseed");
    expect(requests.slice(3)).toEqual([["%2", "post-resync reseed"]]);
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

  it("advances the watermark through a standalone agent event instead of reading it as a gap", () => {
    // A live agent event is an ordered host event that spent a sequence of its
    // own. The native layer used to stamp every agent frame local, so the very
    // next ordered frame looked like a lost one and tore the connection down
    // for the length of any agent run.
    const hub = new TerminalEventHub();
    const received: TerminalEvent[] = [];
    const before = output(1, 1);
    const agent = { kind: "agentService", scope: "claude-code:2f9a", event: { generation: "9", connectionEpoch: "41", notify: true, reason: "blocked", agent: { agentId: "claude-code:2f9a" } }, sequence: 2 } as TerminalEvent;
    const after = resource(3, 2);
    expect(hub.publish(before, () => received.push(before))).toEqual({ kind: "accepted" });
    expect(hub.publish(agent, () => received.push(agent))).toEqual({ kind: "accepted" });
    expect(hub.publish(after, () => received.push(after))).toEqual({ kind: "accepted" });
    expect(received).toEqual([before, agent, after]);
    expect(hub.lastSequence).toBe(3);
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

  // A pane's own consumer used to be unable to see either of the hub's degraded
  // latches: while `awaitingSeed` stands the hub drops that pane's events
  // instead of delivering them, so a lost recovery signal was indistinguishable
  // from a quiet pane. Both latches are now visible and the conflict one is
  // retriable, which is what lets the pane put a time bound on them.
  it("reports the degraded state it is holding a pane in", () => {
    const hub = new TerminalEventHub();
    const health: Array<{ awaitingSeed: boolean; conflictReseedRequested: boolean }> = [];
    hub.subscribePane("%1", () => undefined, (value) => health.push(value));
    expect(health).toEqual([{ awaitingSeed: false, conflictReseedRequested: false }]);

    hub.publish(resource(1, 1, {
      state: "released", requiresSeed: true, recoveryReason: "host recovery pending",
      serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array(),
    }));
    expect(hub.paneHealth("%1")).toEqual({ awaitingSeed: true, conflictReseedRequested: false });
    expect(health.at(-1)).toEqual({ awaitingSeed: true, conflictReseedRequested: false });

    // Dropped output while awaiting a seed is not a change, and must not turn
    // into a notification per chunk.
    hub.publish(output(2, 2));
    expect(health).toHaveLength(2);

    hub.publish(seed(3, 3));
    expect(hub.paneHealth("%1")).toEqual({ awaitingSeed: false, conflictReseedRequested: false });
    expect(health.at(-1)).toEqual({ awaitingSeed: false, conflictReseedRequested: false });
  });

  it("reports a pane mounting straight into inherited seed debt", () => {
    const hub = new TerminalEventHub();
    hub.publish(resource(1, 1, {
      state: "released", requiresSeed: true, recoveryReason: "host recovery pending",
      serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array(),
    }));
    const health: Array<{ awaitingSeed: boolean }> = [];
    hub.subscribePane("%1", () => undefined, (value) => health.push(value));
    expect(health).toEqual([{ awaitingSeed: true, conflictReseedRequested: false }]);
  });

  it("makes the one-shot conflict reseed retriable", () => {
    const requests: string[] = [];
    const hub = new TerminalEventHub((paneId) => requests.push(paneId));
    const health: Array<{ conflictReseedRequested: boolean }> = [];
    hub.subscribePane("%1", () => undefined, (value) => health.push(value));
    hub.publish(output(1, 7));
    hub.publish(seed(2, 3));
    expect(requests).toEqual(["%1"]);
    expect(hub.paneHealth("%1").conflictReseedRequested).toBe(true);

    // Unchanged: a second stale seed is still rate-limited to nothing.
    hub.publish(seed(3, 4));
    expect(requests).toEqual(["%1"]);

    // The pane's watchdog is re-requesting the seed itself, so the latch that
    // exists to bound a storm reopens rather than ending recovery outright.
    hub.retryPaneSeed("%1");
    expect(hub.paneHealth("%1").conflictReseedRequested).toBe(false);
    expect(health.at(-1)?.conflictReseedRequested).toBe(false);
    hub.publish(seed(4, 5));
    expect(requests).toEqual(["%1", "%1"]);
    // Retrying a pane that owes nothing is inert.
    hub.publish(seed(5, 9));
    hub.retryPaneSeed("%1");
    hub.retryPaneSeed("%unknown");
    expect(requests).toEqual(["%1", "%1"]);
  });

  // The ladder that decides whether a pane is still owed a seed reads a byte
  // count today, and a verified resume answer carries no bytes. Reading the
  // count instead of the flag leaves the pane waiting for a seed nobody owes
  // it — the single most likely way to ship a permanently blank pane.
  //
  // Lands with steps 3 and 4 (§4.3).
  it.skip("clears seed debt for a zero-byte answer only when it carries the resume flag", () => {
    const hub = new TerminalEventHub();
    hub.subscribePane("%1", () => undefined);
    hub.publish(resource(1, 1, {
      requiresSeed: true,
      recoveryReason: "host recovery pending",
      serializedSnapshot: new Uint8Array(),
      rawTail: new Uint8Array(),
    }));
    expect(hub.paneHealth("%1").awaitingSeed).toBe(true);

    // An empty answer that is not a resume repairs nothing, and must not
    // cancel the seed this pane is owed.
    hub.publish(resource(2, 2, { serializedSnapshot: new Uint8Array(), rawTail: new Uint8Array() }));
    expect(hub.paneHealth("%1").awaitingSeed).toBe(true);

    // The same zero bytes, carrying the host's verified checkpoint, are the
    // whole recovery: the renderer already holds the screen they continue.
    hub.publish(resumeAnswer(3, 3));
    expect(hub.paneHealth("%1").awaitingSeed).toBe(false);
  });

  it("contains a failing pane health observer like every other observer", () => {
    const failures: string[] = [];
    const hub = new TerminalEventHub(undefined, {}, undefined, (message) => failures.push(message));
    const received: TerminalEvent[] = [];
    hub.subscribePane("%1", (event) => received.push(event), () => {
      throw new Error("watchdog rejected a health report");
    });
    expect(hub.publish(seed(1, 1))).toEqual({ kind: "accepted" });
    expect(received).toEqual([seed(1, 1)]);
    expect(failures.at(-1)).toContain("terminal pane health observer failed");
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
