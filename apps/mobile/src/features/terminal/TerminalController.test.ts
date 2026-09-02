import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostConnection } from "../../protocol/HostConnection";
import { EventKind, HostEventSchema, Operation, PaneResourceSchema, PaneResourceState, ResponseSchema, TerminalBytesSchema, type Envelope, type Request } from "../../protocol/gen/envelope_pb";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../../protocol/testing/fakeTransport";
import { createSessionStore } from "../../store/sessionStore";
import type { ToPageMessage } from "./bridgeMessages";
import { TerminalController, type TerminalSnapshot } from "./TerminalController";
import { TerminalRegistry } from "./terminalRegistry";

const settle = () => vi.advanceTimersByTimeAsync(0);
const SEED = new TextEncoder().encode("\x1b[2J$ ");

function harness() {
  const store = createSessionStore();
  const registry = new TerminalRegistry();
  const transports: FakeTransport[] = [];
  let epoch = 0;
  const connection = new HostConnection({
    dial: async () => {
      const t = new FakeTransport();
      transports.push(t);
      return t;
    },
    appVersion: "t",
    nextConnectionEpoch: () => (epoch += 1),
    store,
    terminals: registry,
    onConnected: () => registry.onConnected(),
    maxBackoffMs: 1000,
  });
  const page: ToPageMessage[] = [];
  const snapshots: TerminalSnapshot[] = [];
  const controller = new TerminalController({
    paneId: "%1",
    sessionId: "$1",
    store,
    registry,
    getConnection: () => connection,
    page: { send: (m) => page.push(m) },
    onChange: (s) => snapshots.push(s),
  });
  const connect = async () => {
    connection.connect();
    await settle();
    const t = transports.at(-1)!;
    t.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: BigInt(epoch), terminalOutputWindowBytes: 1000n }) }, { requestId: 1n }));
    t.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    t.drain();
    return t;
  };
  return { store, registry, transports, connection, page, snapshots, controller, connect, transport: () => transports.at(-1)! };
}

/** Answers the next request on the transport with ok and returns it. */
async function answerNext(transport: FakeTransport): Promise<Request> {
  await settle();
  const [frame] = transport.drain();
  if (frame?.payload.case !== "request") throw new Error(`expected a request, got ${frame?.payload.case}`);
  transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
  await settle();
  return frame.payload.value;
}

function terminalEvent(kind: EventKind, sequence: bigint, data: Uint8Array, generation: bigint, bytes = BigInt(data.byteLength)): Envelope {
  return hostEnvelope({
    case: "event",
    value: create(HostEventSchema, {
      kind,
      terminal: create(TerminalBytesSchema, { paneId: "%1", data, generation }),
      terminalDeliveryBytes: bytes,
      terminalDeliveryRecords: 1n,
    }),
  }, { sequence });
}

describe("TerminalController scrollback paging (§7.6.1)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  async function seeded() {
    const h = harness();
    await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 80, rows: 24 });
    await answerNext(h.transport()); // select
    await answerNext(h.transport()); // resize
    await answerNext(h.transport()); // attach
    await answerNext(h.transport()); // seed request (§7.6 step 1)
    h.transport().feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 1n));
    await settle();
    h.page.length = 0;
    return h;
  }

  function historyEvent(sequence: bigint, rows: string[], historySize: number, known = true): Envelope {
    return hostEnvelope({
      case: "event",
      value: create(HostEventSchema, {
        kind: EventKind.TERMINAL_HISTORY,
        terminal: create(TerminalBytesSchema, {
          paneId: "%1",
          data: new TextEncoder().encode(rows.join("\r\n")),
          historySize,
          historySizeKnown: known,
        }),
      }),
    }, { sequence });
  }

  it("atTop asks for a first page with the page's rows as skip, splices the answer above the retained bytes, and doubles the next page", async () => {
    const h = await seeded();
    const out = new TextEncoder().encode("more");
    h.transport().feed(terminalEvent(EventKind.TERMINAL_OUTPUT, 2n, out, 2n));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 7 });
    await settle();
    const [frame] = h.transport().drain();
    if (frame?.payload.case !== "request") throw new Error("expected the history request");
    expect(frame.payload.value.operation).toBe(Operation.REQUEST_TERMINAL_HISTORY);
    expect(frame.payload.value.scope).toBe("%1");
    expect(frame.payload.value.terminalHistoryLines).toBe(300);
    expect(frame.payload.value.terminalHistorySkipLines).toBe(7);
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
    h.transport().feed(historyEvent(3n, ["one", "two", "three"], 5000));
    await settle();
    const splice = h.page.at(-1);
    if (splice?.t !== "splice") throw new Error(`expected a splice, got ${String(splice?.t)}`);
    expect(atob(splice.hist)).toBe("one\r\ntwo\r\nthree");
    // The tail replays the seed and every output since, in order.
    expect(atob(splice.tail)).toBe("\x1b[2J$ more");
    expect(splice.rowsAdded).toBe(3);
    // The next page doubles, and its splice stacks the older page above the
    // one already held — the buffer is rebuilt whole, so nothing may be left out.
    h.controller.onPageMessage({ t: "atTop", above: 10 });
    await settle();
    const [next] = h.transport().drain();
    if (next?.payload.case !== "request") throw new Error("expected the second history request");
    expect(next.payload.value.terminalHistoryLines).toBe(600);
    expect(next.payload.value.terminalHistorySkipLines).toBe(10);
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: next.requestId }));
    h.transport().feed(historyEvent(4n, ["older-a", "older-b"], 5000));
    await settle();
    const second = h.page.at(-1);
    if (second?.t !== "splice") throw new Error("expected the second splice");
    expect(atob(second.hist)).toBe("older-a\r\nolder-b\r\none\r\ntwo\r\nthree");
    expect(second.rowsAdded).toBe(2);
    expect(atob(second.tail)).toBe("\x1b[2J$ more");
  });

  it("one request in flight; done when the spliced rows reach tmux's history size; an unknown size asks again", async () => {
    const h = await seeded();
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    const [frame] = h.transport().drain();
    if (frame?.payload.case !== "request") throw new Error("expected the history request");
    // A second atTop while the first is unanswered is ignored.
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    expect(h.transport().drain()).toHaveLength(0);
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
    // Probe failed: sizeKnown=false is "ask again", so paging continues…
    h.transport().feed(historyEvent(2n, ["a", "b"], 0, false));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 2 });
    await settle();
    const [second] = h.transport().drain();
    if (second?.payload.case !== "request") throw new Error("expected another request");
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: second.requestId }));
    // …until a known size says the two rows plus these two are everything.
    h.transport().feed(historyEvent(3n, ["c", "d"], 4));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 4 });
    await settle();
    expect(h.transport().drain()).toHaveLength(0);
  });

  it("stops at the scrollback limit and never asks for more rows than fit under it", async () => {
    const h = await seeded();
    h.controller.onPageMessage({ t: "atTop", above: 9_900 });
    await settle();
    const [frame] = h.transport().drain();
    if (frame?.payload.case !== "request") throw new Error("expected the history request");
    expect(frame.payload.value.terminalHistoryLines).toBe(100);
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
    h.transport().feed(historyEvent(2n, Array.from({ length: 100 }, (_, i) => `r${i}`), 50_000));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 10_000 });
    await settle();
    expect(h.transport().drain()).toHaveLength(0);
  });

  it("a late rejection of an older request does not unlatch the one in flight", async () => {
    const h = await seeded();
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    const [first] = h.transport().drain();
    if (first?.payload.case !== "request") throw new Error("expected the first request");
    // A reseed resets the ledger while the first request is unanswered…
    h.transport().feed(terminalEvent(EventKind.TERMINAL_SEED, 2n, SEED, 3n));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    const [second] = h.transport().drain();
    if (second?.payload.case !== "request") throw new Error("expected the second request");
    // …then the host rejects the first one late.
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse({ ok: false, errorCode: "pane_gone", displayMessage: "gone" }) }, { requestId: first.requestId }));
    await settle();
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    expect(h.transport().drain()).toHaveLength(0); // the second is still in flight
  });

  it("discards the clamp row tmux answers an empty history with, and trims a page that ran past the top", async () => {
    const h = await seeded();
    // Nothing above the screen: history_size 0, tmux still answers one row.
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    const [first] = h.transport().drain();
    if (first?.payload.case !== "request") throw new Error("expected the history request");
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: first.requestId }));
    h.transport().feed(historyEvent(2n, ["$ "], 0));
    await settle();
    expect(h.page.filter((m) => m.t === "splice")).toHaveLength(0);
    // Done: no further request for this seed.
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    expect(h.transport().drain()).toHaveLength(0);

    // A page that asked for 300 rows above 5 held, of a 7-row history: only
    // the last 2 rows of the answer are real.
    const g = await seeded();
    g.controller.onPageMessage({ t: "atTop", above: 5 });
    await settle();
    const [req] = g.transport().drain();
    if (req?.payload.case !== "request") throw new Error("expected the history request");
    g.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: req.requestId }));
    g.transport().feed(historyEvent(2n, ["clamp", "clamp", "real-1", "real-2"], 7));
    await settle();
    const splice = g.page.at(-1);
    if (splice?.t !== "splice") throw new Error("expected a splice");
    expect(atob(splice.hist)).toBe("real-1\r\nreal-2");
    g.controller.onPageMessage({ t: "atTop", above: 7 });
    await settle();
    expect(g.transport().drain()).toHaveLength(0); // 5 + 2 = 7: the top was reached
  });

  it("a fresh seed resets the ledger: the retained tail restarts and paging is live again", async () => {
    const h = await seeded();
    h.controller.onPageMessage({ t: "atTop", above: 0 });
    await settle();
    const [frame] = h.transport().drain();
    if (frame?.payload.case !== "request") throw new Error("expected the history request");
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
    // The reseed lands before the answer: the answer has nothing to splice against.
    const reseed = new TextEncoder().encode("\x1b[2Jnew");
    h.transport().feed(terminalEvent(EventKind.TERMINAL_SEED, 2n, reseed, 3n));
    h.transport().feed(historyEvent(3n, ["stale"], 100));
    await settle();
    expect(h.page.filter((m) => m.t === "splice")).toHaveLength(0);
    // Paging works against the new screen, with the new tail.
    h.controller.onPageMessage({ t: "atTop", above: 1 });
    await settle();
    const [again] = h.transport().drain();
    if (again?.payload.case !== "request") throw new Error("expected a request after the reseed");
    h.transport().feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: again.requestId }));
    h.transport().feed(historyEvent(4n, ["h1"], 2)); // 1 held + 1 above
    await settle();
    const splice = h.page.at(-1);
    if (splice?.t !== "splice") throw new Error("expected a splice after the reseed");
    expect(atob(splice.tail)).toBe("\x1b[2Jnew");
  });
});

describe("TerminalController attach lifecycle (§7.6)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("takes focus, inits the page, and after the first size sends select → resize → attach in that order", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    expect(h.store.getState().focusedPaneId).toBe("%1");
    expect(h.page).toEqual([{ t: "init" }]);
    expect(t.drain()).toHaveLength(0); // nothing before the page measured

    h.controller.onPageMessage({ t: "ready" });
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    const select = await answerNext(t);
    expect(select).toMatchObject({ operation: Operation.SELECT_TERMINAL_SESSION, sessionId: "$1" });
    const resize = await answerNext(t);
    expect(resize).toMatchObject({ operation: Operation.RESIZE_TERMINAL, columns: 46, rows: 40 });
    const attach = await answerNext(t);
    expect(attach).toMatchObject({ operation: Operation.ATTACH_TERMINAL, sessionId: "$1", paneIds: ["%1"] });
    // The attach mounts; the seed is asked for explicitly right behind it.
    const seedRequest = await answerNext(t);
    expect(seedRequest).toMatchObject({ operation: Operation.REQUEST_TERMINAL_SEED, scope: "%1" });
    // No SET_TERMINAL_VISIBILITY(true) on mount: the attach is the reveal.
    expect(t.drain()).toHaveLength(0);
    expect(h.controller.snapshot.phase).toBe("attaching");
  });

  it("delivers the seed as a reset+write, charges credit, and acks it within 50 ms", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t);
    await answerNext(t);
    await answerNext(t);
    await answerNext(t); // seed request (§7.6 step 1)
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 5n));
    expect(h.page.at(-1)).toEqual({ t: "seed", b64: Buffer.from(SEED).toString("base64") });
    expect(h.controller.snapshot.phase).toBe("seeded");
    expect(h.controller.generation).toBe(5n);
    await vi.advanceTimersByTimeAsync(50);
    const [ack] = t.drain();
    expect(ack?.payload.case).toBe("terminalOutputAck");
    if (ack?.payload.case !== "terminalOutputAck") throw new Error("unreachable");
    expect(ack.payload.value).toMatchObject({ connectionEpoch: 1n, cumulativeBytes: BigInt(SEED.byteLength), cumulativeRecords: 1n });

    // Output after the seed is written in order and tracked by generation.
    const out = new TextEncoder().encode("hi\r\n");
    t.feed(terminalEvent(EventKind.TERMINAL_OUTPUT, 2n, out, 6n));
    expect(h.page.at(-1)).toEqual({ t: "out", b64: Buffer.from(out).toString("base64") });
    expect(h.controller.generation).toBe(6n);
    // A seed older than what was written is stale and dropped — but still acked.
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 3n, SEED, 4n));
    expect(h.page.at(-1)).toEqual({ t: "out", b64: Buffer.from(out).toString("base64") });
    await vi.advanceTimersByTimeAsync(50);
    const [ack2] = t.drain();
    if (ack2?.payload.case !== "terminalOutputAck") throw new Error("expected an ack");
    expect(ack2.payload.value).toMatchObject({ cumulativeBytes: BigInt(SEED.byteLength * 2 + out.byteLength), cumulativeRecords: 3n });
  });

  it("acks a seed larger than a quarter of the window immediately", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t);
    await answerNext(t);
    await answerNext(t);
    await answerNext(t); // seed request (§7.6 step 1)
    const big = new Uint8Array(300); // window is 1000 bytes; 25 % is 250
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, big, 1n));
    const [ack] = t.drain();
    expect(ack?.payload.case).toBe("terminalOutputAck");
  });

  it("asks for a seed once after 5 s and shows the hint after 10 s", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t);
    await answerNext(t);
    await answerNext(t);
    await answerNext(t); // seed request (§7.6 step 1)
    await vi.advanceTimersByTimeAsync(4_999);
    expect(t.drain()).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    const [request] = t.drain();
    if (request?.payload.case !== "request") throw new Error("expected REQUEST_TERMINAL_SEED");
    expect(request.payload.value).toMatchObject({ operation: Operation.REQUEST_TERMINAL_SEED, scope: "%1" });
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: request.requestId }));
    expect(h.controller.snapshot.phase).toBe("attaching");
    await vi.advanceTimersByTimeAsync(5_000);
    expect(h.controller.snapshot.phase).toBe("noOutput");
    expect(t.drain()).toHaveLength(0); // only one retry
    // A late seed still clears the hint.
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 1n));
    expect(h.controller.snapshot.phase).toBe("seeded");
  });

  it("debounces resizes 150 ms and sends only the latest grid", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t);
    await answerNext(t);
    await answerNext(t);
    await answerNext(t); // seed request (§7.6 step 1)
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 20 });
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 22 });
    await vi.advanceTimersByTimeAsync(149);
    expect(t.drain()).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    const [resize] = t.drain();
    if (resize?.payload.case !== "request") throw new Error("expected a resize");
    expect(resize.payload.value).toMatchObject({ operation: Operation.RESIZE_TERMINAL, columns: 46, rows: 22 });
    // Same grid again: nothing.
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 22 });
    await vi.advanceTimersByTimeAsync(200);
    expect(t.drain()).toHaveLength(0);
  });

  it("on stop clears focus first, then hides with the connection epoch and the last generation", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t);
    await answerNext(t);
    await answerNext(t);
    await answerNext(t); // seed request (§7.6 step 1)
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 9n));
    const stopped = h.controller.stop();
    expect(h.store.getState().focusedPaneId).toBeUndefined();
    await settle();
    const frames = t.drain().filter((f) => f.payload.case === "request");
    expect(frames).toHaveLength(1);
    const hide = frames[0]!;
    if (hide.payload.case !== "request") throw new Error("unreachable");
    expect(hide.payload.value).toMatchObject({
      operation: Operation.SET_TERMINAL_VISIBILITY,
      scope: "%1",
      visible: false,
      terminalEpoch: 1n,
      terminalGenerationCutoff: 9n,
    });
    expect(hide.payload.value.data.byteLength).toBe(0);
    // The hide's PANE_RESOURCE{RELEASED, requiresSeed} must not turn into a reveal
    // now that the pane is unfocused — and its charge is still acknowledged.
    t.feed(hostEnvelope({
      case: "event",
      value: create(HostEventSchema, {
        kind: EventKind.PANE_RESOURCE,
        paneResource: create(PaneResourceSchema, { paneId: "%1", state: PaneResourceState.RELEASED, requiresSeed: true }),
        terminalDeliveryRecords: 1n,
      }),
    }, { sequence: 2n }));
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: hide.requestId }));
    await stopped;
    await vi.advanceTimersByTimeAsync(50);
    const after = t.drain();
    expect(after.filter((f) => f.payload.case === "request")).toHaveLength(0);
    const ack = after.find((f) => f.payload.case === "terminalOutputAck");
    if (ack?.payload.case !== "terminalOutputAck") throw new Error("expected the PANE_RESOURCE charge to be acked");
    expect(ack.payload.value.cumulativeRecords).toBe(2n);
    // Events after stop no longer reach the page.
    t.feed(terminalEvent(EventKind.TERMINAL_OUTPUT, 3n, SEED, 10n));
    expect(h.page.filter((m) => m.t === "out")).toHaveLength(0);
  });

  it("re-runs select → resize → attach on reconnect while mounted", async () => {
    const h = harness();
    const t1 = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 40, rows: 30 });
    await answerNext(t1);
    await answerNext(t1);
    await answerNext(t1);
    await answerNext(t1); // seed request (§7.6 step 1)
    t1.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 1n));
    t1.closeFromRemote({ reason: "networkLost" });
    expect(h.store.getState().connection.state).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1_000);
    const t2 = h.transport();
    expect(t2).not.toBe(t1);
    t2.drain(); // ClientHello + Subscribe
    t2.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 2n, terminalOutputWindowBytes: 1000n }) }, { requestId: 1n }));
    t2.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    const ops = [await answerNext(t2), await answerNext(t2), await answerNext(t2), await answerNext(t2)].map((r) => r.operation);
    expect(ops).toEqual([Operation.SELECT_TERMINAL_SESSION, Operation.RESIZE_TERMINAL, Operation.ATTACH_TERMINAL, Operation.REQUEST_TERMINAL_SEED]);
    expect(h.store.getState().focusedPaneId).toBe("%1");
    // The hide on the new connection carries the new epoch.
    void h.controller.stop();
    await settle();
    const [hide] = t2.drain();
    if (hide?.payload.case !== "request") throw new Error("expected a hide");
    expect(hide.payload.value.terminalEpoch).toBe(2n);
  });

  it("marks the pane exited on TERMINAL_EXIT for its session, and stays silent for other sessions", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    t.feed(hostEnvelope({ case: "event", value: create(HostEventSchema, { kind: EventKind.TERMINAL_EXIT, scope: "$2", detail: "other" }) }, { sequence: 1n }));
    expect(h.controller.snapshot.phase).toBe("preparing");
    t.feed(hostEnvelope({ case: "event", value: create(HostEventSchema, { kind: EventKind.TERMINAL_EXIT, scope: "$1", detail: "control client failed" }) }, { sequence: 2n }));
    expect(h.controller.snapshot.phase).toBe("exited");
  });

  it("sends each input immediately as TERMINAL_INPUT scoped to the pane", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    const pending = h.controller.sendInput(Uint8Array.of(0x03));
    const [frame] = t.drain();
    if (frame?.payload.case !== "request") throw new Error("expected input");
    expect(frame.payload.value).toMatchObject({ operation: Operation.TERMINAL_INPUT, scope: "%1" });
    expect(Array.from(frame.payload.value.data)).toEqual([0x03]);
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: frame.requestId }));
    await expect(pending).resolves.toBeUndefined();
  });
});

describe("TerminalController stop during attach", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("hides the pane once the in-flight attach completes", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t); // select
    await answerNext(t); // resize
    await settle();
    const [attach] = t.drain();
    if (attach?.payload.case !== "request") throw new Error("expected attach");
    expect(attach.payload.value.operation).toBe(Operation.ATTACH_TERMINAL);
    const stopped = h.controller.stop();
    expect(h.store.getState().focusedPaneId).toBeUndefined();
    await settle();
    expect(t.drain()).toHaveLength(0); // nothing until the attach answers
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: attach.requestId }));
    await settle();
    const [hide] = t.drain();
    if (hide?.payload.case !== "request") throw new Error("expected a hide after the attach settled");
    expect(hide.payload.value).toMatchObject({ operation: Operation.SET_TERMINAL_VISIBILITY, visible: false, scope: "%1", terminalEpoch: 1n });
    await stopped;
  });
});

describe("TerminalController attach failure", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("retries the attach after 2 s when a step is refused", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await settle();
    const [select] = t.drain();
    t.feed(hostEnvelope({ case: "response", value: create(ResponseSchema, { ok: false, errorCode: "terminal_resize_rejected", displayMessage: "no visible session control client" }) }, { requestId: select!.requestId }));
    await settle();
    expect(h.controller.snapshot.lastError).toContain("no visible session control client");
    expect(t.drain()).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(2_000);
    const ops = [await answerNext(t), await answerNext(t), await answerNext(t)].map((r) => r.operation);
    expect(ops).toEqual([Operation.SELECT_TERMINAL_SESSION, Operation.RESIZE_TERMINAL, Operation.ATTACH_TERMINAL]);
  });

  it("stops sending the remaining steps once stopped mid-select", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await settle();
    const [select] = t.drain();
    void h.controller.stop();
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: select!.requestId }));
    await settle();
    expect(t.drain()).toHaveLength(0);
  });
});

describe("TerminalController successor and reconnect", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("skips the late hide when a successor controller already owns the pane", async () => {
    const h = harness();
    const t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t); // select
    await answerNext(t); // resize
    await settle();
    const [attach] = t.drain();
    if (attach?.payload.case !== "request") throw new Error("expected attach");
    const stopped = h.controller.stop();
    const successor = new TerminalController({
      paneId: "%1",
      sessionId: "$1",
      store: h.store,
      registry: h.registry,
      getConnection: () => h.connection,
      page: { send: () => {} },
      onChange: () => {},
    });
    successor.start();
    t.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: attach.requestId }));
    await settle();
    await stopped;
    const ops = t.drain().map((f) => f.payload.case === "request" ? f.payload.value.operation : undefined);
    expect(ops).not.toContain(Operation.SET_TERMINAL_VISIBILITY);
    await successor.stop();
  });

  it("resets the generation cutoff on reconnect so a restarted daemon's seed is not stale", async () => {
    const h = harness();
    let t = await h.connect();
    h.controller.start();
    h.controller.onPageMessage({ t: "size", cols: 46, rows: 40 });
    await answerNext(t); // select
    await answerNext(t); // resize
    await answerNext(t); // attach
    await answerNext(t); // seed request (§7.6 step 1)
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 50n));
    await settle();
    expect(h.controller.snapshot.phase).toBe("seeded");
    expect(h.controller.generation).toBe(50n);
    t.closeFromRemote({ reason: "networkLost" });
    await vi.advanceTimersByTimeAsync(1_000);
    t = h.transport();
    t.drain();
    t.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 2n, terminalOutputWindowBytes: 1000n }) }, { requestId: 1n }));
    t.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await answerNext(t); // select
    await answerNext(t); // resize
    await answerNext(t); // attach
    await answerNext(t); // seed request (§7.6 step 1)
    h.page.length = 0;
    t.feed(terminalEvent(EventKind.TERMINAL_SEED, 1n, SEED, 2n));
    await settle();
    expect(h.page.map((m) => m.t)).toContain("seed");
  });
});
