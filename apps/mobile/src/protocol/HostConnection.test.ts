import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AgentEventSchema,
  AgentLifecycleState,
  AgentRouteSchema,
  AgentRecordSchema,
  EventKind,
  HostEventSchema,
  Operation,
  TerminalBytesSchema,
  VoiceEventSchema,
  VoiceProvisionProgressSchema,
  VoiceResponseSchema,
  VoiceSpeechSchema,
  type Envelope,
  type HostEvent,
} from "./gen/envelope_pb";
import { HostConnection, HostError, type HostConnectionOptions } from "./HostConnection";
import { jsBackgroundTimer } from "./backgroundTimer";
import { terminalInput, voiceTranscribe } from "./requests";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "./testing/fakeTransport";
import { createSessionStore, type SessionStore } from "../store/sessionStore";
import { createNotificationAttention } from "../features/notifications/attention";

interface Harness {
  connection: HostConnection;
  store: SessionStore;
  transports: FakeTransport[];
  log: string[];
  seeds: Array<{ paneId: string; bytes: Uint8Array; generation: bigint }>;
  outputs: Array<{ paneId: string; bytes: Uint8Array; generation: bigint }>;
  dials: number;
  epoch: number;
}

function harness(overrides: Partial<HostConnectionOptions> = {}): Harness {
  const store = createSessionStore();
  const state: Harness = {
    connection: undefined as unknown as HostConnection,
    store,
    transports: [],
    log: [],
    seeds: [],
    outputs: [],
    dials: 0,
    epoch: 0,
  };
  state.connection = new HostConnection({
    dial: async () => {
      state.dials += 1;
      const transport = new FakeTransport();
      state.transports.push(transport);
      return transport;
    },
    nextConnectionEpoch: () => (state.epoch += 1),
    store,
    host: { id: "h", label: "Dev box", host: "dev.local", port: 22, user: "dev" },
    terminals: {
      seed: (paneId, bytes, generation) => state.seeds.push({ paneId, bytes, generation }),
      output: (paneId, bytes, generation) => state.outputs.push({ paneId, bytes, generation }),
    },
    log: (line) => state.log.push(line),
    ...overrides,
  });
  return state;
}

/** Lets the async dial settle. */
const settle = () => vi.advanceTimersByTimeAsync(0);

function event(kind: EventKind, sequence: bigint, extra: MessageInitShape<typeof HostEventSchema> = {}): Envelope {
  return hostEnvelope({ case: "event", value: create(HostEventSchema, { kind, ...extra }) }, { sequence });
}

async function connectHappily(h: Harness, acceptedSequence = 0n): Promise<FakeTransport> {
  h.connection.connect();
  await settle();
  const transport = h.transports.at(-1)!;
  transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
  transport.feed(hostEnvelope(
    { case: "response", value: okResponse({ snapshot: topologySnapshot(), acceptedSequence }) },
    { requestId: 2n },
  ));
  return transport;
}

describe("handshake (§7.3)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("pipelines ClientHello and Subscribe, then reaches connected with the snapshot applied", async () => {
    const h = harness();
    h.connection.connect();
    expect(h.store.getState().connection.state).toBe("sshConnecting");
    await settle();
    expect(h.store.getState().connection.state).toBe("handshaking");
    const transport = h.transports[0]!;
    const [hello, subscribe] = transport.drain();
    expect(hello?.requestId).toBe(1n);
    expect(hello?.protocolMajor).toBe(4);
    expect(hello?.payload.case).toBe("clientHello");
    if (hello?.payload.case !== "clientHello") throw new Error("unreachable");
    expect(hello.payload.value.bulkConnection).toBe(false);
    expect(hello.payload.value.connectionEpoch).toBe(1n);
    expect(subscribe?.requestId).toBe(2n);
    if (subscribe?.payload.case !== "request") throw new Error("unreachable");
    expect(subscribe.payload.value.operation).toBe(Operation.SUBSCRIBE);
    expect(subscribe.payload.value.scope).toBe("full");

    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ helperVersion: "" }) }, { requestId: 1n }));
    expect(h.store.getState().connection.state).toBe("handshaking");
    transport.feed(hostEnvelope(
      { case: "response", value: okResponse({ snapshot: topologySnapshot(), acceptedSequence: 3n }) },
      { requestId: 2n },
    ));
    const state = h.store.getState();
    expect(state.connection.state).toBe("connected");
    expect(state.serverIdentity).toBe("server-a");
    expect(state.topologyGeneration).toBe(1n);
    expect(Object.keys(state.sessions)).toEqual(["$1"]);
    expect(state.panes["%1"]?.currentPath).toBe("/home/u");
    expect(h.connection.serverHello?.terminalOutputWindowBytes).toBe(BigInt(2 * 1024 * 1024));
  });

  it("refuses a protocol major mismatch with the §7.3 copy and does not retry", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    h.transports[0]!.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n, protocolMajor: 5 }));
    expect(h.store.getState().connection).toMatchObject({
      state: "incompatible",
      message: "This host's Muxflow helper speaks protocol v5; this app needs v4. Update the app or the helper from Muxflow desktop.",
    });
    expect(h.transports[0]!.closed).toBe(true);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.dials).toBe(1);
  });

  it("refuses control admission without terminal output flow control", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    h.transports[0]!.feed(hostEnvelope({ case: "serverHello", value: serverHello({ terminalOutputWindowBytes: 0n }) }, { requestId: 1n }));
    expect(h.store.getState().connection.state).toBe("failed");
    expect(h.transports[0]!.closed).toBe(true);
  });

  it("fails without retry when the first frame is not a ServerHello", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    h.transports[0]!.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: 2n }));
    expect(h.store.getState().connection).toMatchObject({ state: "failed", message: "host did not return ServerHello" });
  });

  it("fails on a refused Subscribe and on a server identity that changed mid-handshake", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    h.transports[0]!.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    h.transports[0]!.feed(hostEnvelope(
      { case: "response", value: okResponse({ ok: false, errorCode: "boom", displayMessage: "no tmux" }) },
      { requestId: 2n },
    ));
    expect(h.store.getState().connection).toMatchObject({ state: "failed", message: "boom: no tmux" });

    const g = harness();
    g.connection.connect();
    await settle();
    g.transports[0]!.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    g.transports[0]!.feed(hostEnvelope(
      { case: "response", value: okResponse({ snapshot: topologySnapshot({ serverIdentity: "server-b" }) }) },
      { requestId: 2n },
    ));
    expect(g.store.getState().connection.state).toBe("failed");
  });

  it("buffers events that beat the Subscribe response and drops those at or below the barrier", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    const transport = h.transports[0]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    // Sequence 1..2 are covered by the snapshot barrier; 3 is not.
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 1n));
    transport.feed(event(EventKind.TERMINAL_OUTPUT, 2n, {
      terminal: create(TerminalBytesSchema, { paneId: "%1", data: new Uint8Array([1]), generation: 1n }),
    }));
    transport.feed(event(EventKind.TERMINAL_OUTPUT, 3n, {
      terminal: create(TerminalBytesSchema, { paneId: "%1", data: new Uint8Array([2]), generation: 2n }),
    }));
    expect(h.store.getState().connection.state).toBe("handshaking");
    transport.feed(hostEnvelope(
      { case: "response", value: okResponse({ snapshot: topologySnapshot(), acceptedSequence: 2n }) },
      { requestId: 2n },
    ));
    expect(h.store.getState().connection.state).toBe("connected");
    expect(h.outputs.map((o) => Array.from(o.bytes))).toEqual([[2]]);
    // And the watermark moved on, so 4 is the next expected sequence.
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 4n));
    expect(h.store.getState().connection.state).toBe("connected");
  });
});

describe("ordered events (§7.4)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("accepts sequence 0 and the next number, and reconnects on a gap", async () => {
    const h = harness();
    const transport = await connectHappily(h, 5n);
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 0n));
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 6n));
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 7n));
    expect(h.store.getState().connection.state).toBe("connected");
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 9n));
    expect(h.log.some((line) => line.includes("protocol.gap expected=8 got=9"))).toBe(true);
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1 });
    expect(transport.closed).toBe(true);
    // Backoff 2^0 = 1 s, then a brand-new connection with a new epoch.
    await vi.advanceTimersByTimeAsync(999);
    expect(h.dials).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.dials).toBe(2);
    const [hello] = h.transports[1]!.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("unreachable");
    expect(hello.payload.value.connectionEpoch).toBe(2n);
  });

  it("treats a duplicate sequence as a gap", async () => {
    const h = harness();
    const transport = await connectHappily(h, 0n);
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 1n));
    transport.feed(event(EventKind.TOPOLOGY_DIRTY, 1n));
    expect(h.store.getState().connection.state).toBe("reconnecting");
  });

  it("applies TOPOLOGY_SNAPSHOT, AGENT_STATE and RESYNC_REQUIRED per the table", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.feed(event(EventKind.TOPOLOGY_SNAPSHOT, 1n, {
      snapshot: topologySnapshot({ generation: 9n, sessions: [{ id: "$1", name: "renamed", windowCount: 1, attachedClients: 0, order: 0 }] }),
    }));
    expect(h.store.getState().topologyGeneration).toBe(9n);
    expect(h.store.getState().sessions["$1"]?.name).toBe("renamed");
    transport.feed(event(EventKind.AGENT_STATE, 2n, {
      agent: create(AgentEventSchema, {
        agent: create(AgentRecordSchema, { agentId: "a1", adapterId: "codex", displayName: "Codex", lifecycle: AgentLifecycleState.BLOCKED, stateGeneration: 1n, present: true }),
      }),
    }));
    expect(h.store.getState().agents["a1"]?.lifecycle).toBe("blocked");
    transport.feed(event(EventKind.RESYNC_REQUIRED, 3n));
    expect(h.store.getState().connection.state).toBe("reconnecting");
  });

  it("reports a tmux server replacement delivered on the live topology stream", async () => {
    const replacements: string[] = [];
    const h = harness({ onServerIdentityChanged: (identity) => replacements.push(identity) });
    const transport = await connectHappily(h);

    transport.feed(event(EventKind.TOPOLOGY_SNAPSHOT, 1n, {
      snapshot: topologySnapshot({ serverIdentity: "server-b", generation: 9n }),
    }));

    expect(h.store.getState().serverIdentity).toBe("server-b");
    expect(replacements).toEqual(["server-b"]);
  });

  it("delivers a same-event identity promotion before the replacement lifecycle transition", async () => {
    const calls: string[] = [];
    const attention = createNotificationAttention();
    attention.focusAgent("manual");
    const h = harness({
      onAgentIdentityPromotion: (promotion) => {
        attention.promoteAgent(promotion.retiredAgentIds[0]!, promotion.agent.id);
        calls.push(`promote:${promotion.retiredAgentIds.join(",")}->${promotion.agent.id}`);
      },
      onAgentTransition: (transition) => calls.push(`transition:${transition.next.id}:${transition.next.lifecycle}:viewing=${attention.viewedAgentId()}`),
    });
    const transport = await connectHappily(h);
    const route = create(AgentRouteSchema, { sessionId: "$1", windowId: "@1", paneId: "%7" });
    transport.feed(event(EventKind.AGENT_STATE, 1n, {
      agent: create(AgentEventSchema, {
        agent: create(AgentRecordSchema, { agentId: "manual", adapterId: "codex", lifecycle: AgentLifecycleState.IDLE, stateGeneration: 1n, present: true, route }),
      }),
    }));
    calls.length = 0;
    transport.feed(event(EventKind.AGENT_STATE, 2n, {
      agent: create(AgentEventSchema, {
        agent: create(AgentRecordSchema, { agentId: "native", adapterId: "codex", nativeSessionId: "native-session", lifecycle: AgentLifecycleState.WORKING, stateGeneration: 2n, present: true, route }),
        retiredAgentIds: ["manual"],
      }),
    }));

    expect(calls).toEqual(["promote:manual->native", "transition:native:working:viewing=native"]);
    expect(h.store.getState().agents.manual).toBeUndefined();
    expect(h.store.getState().agents.native?.route.paneId).toBe("%7");
  });

  it("does not promote for stale replacement events", async () => {
    const promotions: string[] = [];
    const h = harness({ onAgentIdentityPromotion: (promotion) => promotions.push(promotion.agent.id) });
    const transport = await connectHappily(h);
    const route = create(AgentRouteSchema, { sessionId: "$1", windowId: "@1", paneId: "%7" });
    transport.feed(event(EventKind.AGENT_STATE, 1n, {
      agent: create(AgentEventSchema, { agent: create(AgentRecordSchema, { agentId: "native", adapterId: "codex", stateGeneration: 5n, present: true, route }) }),
    }));
    transport.feed(event(EventKind.AGENT_STATE, 2n, {
      agent: create(AgentEventSchema, {
        agent: create(AgentRecordSchema, { agentId: "native", adapterId: "codex", stateGeneration: 4n, present: true, route }),
        retiredAgentIds: ["manual"],
      }),
    }));
    expect(promotions).toEqual([]);
  });

  it("does not promote between unrelated native sessions that reuse one pane", async () => {
    const promotions: string[] = [];
    const h = harness({ onAgentIdentityPromotion: (promotion) => promotions.push(promotion.agent.id) });
    const transport = await connectHappily(h);
    const route = create(AgentRouteSchema, { sessionId: "$1", windowId: "@1", paneId: "%7" });
    transport.feed(event(EventKind.AGENT_STATE, 1n, {
      agent: create(AgentEventSchema, { agent: create(AgentRecordSchema, { agentId: "native-a", adapterId: "codex", nativeSessionId: "session-a", stateGeneration: 1n, present: true, route }) }),
    }));
    transport.feed(event(EventKind.AGENT_STATE, 2n, {
      agent: create(AgentEventSchema, {
        agent: create(AgentRecordSchema, { agentId: "native-b", adapterId: "codex", nativeSessionId: "session-b", stateGeneration: 2n, present: true, route }),
        retiredAgentIds: ["native-a"],
      }),
    }));

    expect(promotions).toEqual([]);
    expect(h.store.getState().agents["native-a"]).toBeUndefined();
    expect(h.store.getState().agents["native-b"]?.nativeSessionId).toBe("session-b");
  });

  it("routes VOICE_PROVISION and VOICE_REPLY to onVoiceEvent with their payloads intact", async () => {
    const received: HostEvent[] = [];
    const h = harness({ onVoiceEvent: (event) => received.push(event) });
    const transport = await connectHappily(h);
    transport.feed(event(EventKind.VOICE_PROVISION, 1n, {
      scope: "voice",
      voice: create(VoiceEventSchema, {
        provision: create(VoiceProvisionProgressSchema, { operationId: "prov-1", phase: "downloading", transferredBytes: 1024n, totalBytes: 671088640n }),
      }),
    }));
    transport.feed(event(EventKind.VOICE_REPLY, 2n, {
      voice: create(VoiceEventSchema, {
        reply: create(VoiceSpeechSchema, { serverIdentity: "server-a", paneId: "%1", displayMarkdown: "**Done.**", speechText: "Done.", audio: new Uint8Array([0xff, 0xfb]), audioMime: "audio/mpeg", stateGeneration: 4n }),
      }),
    }));
    expect(received.map((e) => e.kind)).toEqual([EventKind.VOICE_PROVISION, EventKind.VOICE_REPLY]);
    expect(received[0]?.voice?.provision?.phase).toBe("downloading");
    expect(received[0]?.voice?.provision?.totalBytes).toBe(671088640n);
    expect(received[1]?.voice?.reply?.paneId).toBe("%1");
    expect(received[1]?.voice?.reply?.audio).toEqual(new Uint8Array([0xff, 0xfb]));
    // Neither is a file or agent event, and neither is logged as ignored.
    expect(h.log.some((line) => line.startsWith("event.ignored"))).toBe(false);
    expect(h.store.getState().agents["a1"]).toBeUndefined();
  });

  it("carries Response.voice on a refused voice request, so the caller can read operationId and retryable", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.drain();
    const refused = h.connection.request(voiceTranscribe("utt-1", new Uint8Array([1, 2, 3]), "audio/mp4"));
    const [sent] = transport.drain();
    expect(sent?.requestId).toBe(3n);
    transport.feed(hostEnvelope({
      case: "response",
      value: okResponse({
        ok: false,
        errorCode: "voice_model_missing",
        displayMessage: "Voice is not set up on this host yet",
        voice: create(VoiceResponseSchema, { operationId: "utt-1", retryable: false }),
      }),
    }, { requestId: 3n }));
    const error = await refused.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(HostError);
    expect((error as HostError).code).toBe("voice_model_missing");
    expect((error as HostError).voice?.operationId).toBe("utt-1");
    expect((error as HostError).voice?.retryable).toBe(false);
  });

  it("answers TERMINAL_RESNAPSHOT_REQUIRED with a scoped REQUEST_TERMINAL_SEED", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.drain();
    transport.feed(event(EventKind.TERMINAL_RESNAPSHOT_REQUIRED, 1n, { scope: "%1" }));
    const [seed] = transport.drain();
    if (seed?.payload.case !== "request") throw new Error("unreachable");
    expect(seed.payload.value.operation).toBe(Operation.REQUEST_TERMINAL_SEED);
    expect(seed.payload.value.scope).toBe("%1");
    expect(seed.requestId).toBe(3n);
  });

  it("delivers seeds and output and acks their charge with the connection epoch", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.drain();
    transport.feed(event(EventKind.TERMINAL_SEED, 1n, {
      terminal: create(TerminalBytesSchema, { paneId: "%1", data: new Uint8Array([7, 8]), generation: 4n }),
      terminalDeliveryBytes: 2n,
      terminalDeliveryRecords: 1n,
    }));
    expect(h.seeds).toEqual([{ paneId: "%1", bytes: new Uint8Array([7, 8]), generation: 4n }]);
    await vi.advanceTimersByTimeAsync(50);
    const [ack] = transport.drain();
    expect(ack?.requestId).toBe(0n);
    if (ack?.payload.case !== "terminalOutputAck") throw new Error("unreachable");
    expect(ack.payload.value).toMatchObject({ connectionEpoch: 1n, cumulativeBytes: 2n, cumulativeRecords: 1n });
  });
});

describe("requests (§7.5) and close policy (§7.2)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("settles a request on its response, rejects on ok=false, and times out at 20 s", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.drain();
    const first = h.connection.request(terminalInput("%1", new Uint8Array([104])));
    const second = h.connection.request(terminalInput("%1", new Uint8Array([105])));
    const third = h.connection.request(terminalInput("%1", new Uint8Array([106])));
    const sent = transport.drain();
    expect(sent.map((frame) => frame.requestId)).toEqual([3n, 4n, 5n]);
    transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: 3n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ ok: false, errorCode: "terminal_input_rejected", displayMessage: "nope" }) }, { requestId: 4n }));
    await expect(first).resolves.toMatchObject({ ok: true });
    await expect(second).rejects.toBeInstanceOf(HostError);
    const timeout = expect(third).rejects.toThrow("The host didn't answer in time.");
    await vi.advanceTimersByTimeAsync(20_000);
    await timeout;
    // One late answer fails only its request: a slow link is not a dead lane.
    expect(transport.closed).toBe(false);
    expect(h.store.getState().connection).toMatchObject({ state: "connected" });
  });

  it("drops the lane only after three unanswered requests in a row and 15 s of silence", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.drain();
    const send = () => h.connection.request(terminalInput("%1", new Uint8Array([1])));
    // Two misses back to back: still connected.
    const miss1 = expect(send()).rejects.toThrow("The host didn't answer in time.");
    await vi.advanceTimersByTimeAsync(20_000);
    await miss1;
    const miss2 = expect(send()).rejects.toThrow("The host didn't answer in time.");
    await vi.advanceTimersByTimeAsync(20_000);
    await miss2;
    expect(transport.closed).toBe(false);
    // An answer in between resets the count.
    const answered = send();
    transport.feed(hostEnvelope({ case: "response", value: okResponse() }, { requestId: transport.drain().at(-1)!.requestId }));
    await expect(answered).resolves.toMatchObject({ ok: true });
    const miss3 = expect(send()).rejects.toThrow("The host didn't answer in time.");
    await vi.advanceTimersByTimeAsync(20_000);
    await miss3;
    expect(transport.closed).toBe(false);
    // Three in a row with no answer for over 15 s: the lane is stalled.
    for (const _ of [1, 2]) {
      const miss = expect(send()).rejects.toThrow("The host didn't answer in time.");
      await vi.advanceTimersByTimeAsync(20_000);
      await miss;
    }
    expect(transport.closed).toBe(true);
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1, message: "host stopped answering; reconnecting" });
  });

  it("reconnects with exponential backoff on network loss and resets after 60 s connected", async () => {
    // The stable timer has to run on the injected clock too: frozen in the
    // background, it would never reset the exponent there. Record what is armed.
    const armed: number[] = [];
    const h = harness({
      reconnectTimer: {
        set: (delayMs, fn) => {
          armed.push(delayMs);
          return jsBackgroundTimer.set(delayMs, fn);
        },
        clear: (handle) => jsBackgroundTimer.clear(handle),
      },
    });
    let transport = await connectHappily(h);
    expect(armed).toEqual([20_000, 60_000]);
    transport.closeFromRemote({ reason: "networkLost" });
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1, message: "Connection lost." });
    await vi.advanceTimersByTimeAsync(1000);
    expect(h.dials).toBe(2);
    h.transports[1]!.closeFromRemote({ reason: "networkLost" });
    expect(h.store.getState().connection.attempt).toBe(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(h.dials).toBe(3);
    transport = h.transports[2]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    expect(h.store.getState().connection).toMatchObject({ state: "connected", attempt: 0 });
    // Before 60 s of stability the backoff exponent is kept: the next delay is 4 s.
    transport.closeFromRemote({ reason: "exited", exitCode: 1, message: "bridge: daemon went away" });
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 3, message: "bridge: daemon went away" });
    await vi.advanceTimersByTimeAsync(3999);
    expect(h.dials).toBe(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.dials).toBe(4);
    transport = h.transports[3]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await vi.advanceTimersByTimeAsync(60_000);
    transport.closeFromRemote({ reason: "networkLost" });
    // After 60 s connected the exponent is back to 0: 1 s again.
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1 });
    expect(armed.filter((delayMs) => delayMs === 60_000)).toHaveLength(3);
  });

  it("runs the backoff on the injected background timer and clears it on disconnect()", async () => {
    // A fake timer that fires only when told to: the reconnect must not lean on setTimeout.
    const armed: Array<{ delayMs: number; fn: () => void; token: string }> = [];
    const cleared: string[] = [];
    let tokens = 0;
    const timer = {
      set: (delayMs: number, fn: () => void) => {
        const token = `w${++tokens}`;
        armed.push({ delayMs, fn, token });
        return { token };
      },
      clear: (handle: { token: string }) => {
        cleared.push(handle.token);
      },
    };
    const store = createSessionStore();
    const h = harness();
    const connection = new HostConnection({
      dial: async () => {
        h.dials += 1;
        const transport = new FakeTransport();
        h.transports.push(transport);
        return transport;
      },
      nextConnectionEpoch: () => (h.epoch += 1),
      store,
      host: { id: "h", label: "Dev box", host: "dev.local", port: 22, user: "dev" },
      log: () => undefined,
      reconnectTimer: timer,
    });
    connection.connect();
    await settle();
    // The handshake deadline is on the same clock and is cleared once the handshake completes.
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000]);
    let transport = h.transports[0]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    expect(store.getState().connection.state).toBe("connected");
    expect(cleared).toEqual(["w1"]);
    // The stable timer is armed on the same clock and cleared with the connection.
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000, 60_000]);

    transport.closeFromRemote({ reason: "networkLost" });
    expect(store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1 });
    expect(cleared).toEqual(["w1", "w2"]);
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000, 60_000, 1000]);
    // The JS clock advancing does nothing on its own.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.dials).toBe(1);

    armed[2]!.fn();
    await settle();
    expect(h.dials).toBe(2);
    expect(store.getState().connection.state).toBe("handshaking");
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000, 60_000, 1000, 20_000]);
    transport = h.transports[1]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    expect(store.getState().connection.state).toBe("connected");
    expect(cleared).toEqual(["w1", "w2", "w4"]);
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000, 60_000, 1000, 20_000, 60_000]);

    // The stable wake arriving resets the exponent: the next backoff is 1 s again, not 2 s.
    armed[4]!.fn();
    transport.closeFromRemote({ reason: "networkLost" });
    expect(store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1 });
    expect(armed.map((entry) => entry.delayMs)).toEqual([20_000, 60_000, 1000, 20_000, 60_000, 1000]);
    connection.disconnect();
    expect(cleared).toEqual(["w1", "w2", "w4", "w6"]);
    expect(store.getState().connection.state).toBe("idle");
    // A wake the native side had already sent by the time it was cancelled is ignored.
    armed[5]!.fn();
    await settle();
    expect(h.dials).toBe(2);
  });

  it("does not retry after auth failure or exit code 127", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.closeFromRemote({ reason: "authFailed" });
    expect(h.store.getState().connection.state).toBe("failed");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.dials).toBe(1);

    const g = harness();
    g.connection.connect();
    await settle();
    g.transports[0]!.closeFromRemote({ reason: "exited", exitCode: 127 });
    expect(g.store.getState().connection).toMatchObject({
      state: "failed",
      message: "muxflow-host isn't installed on this host. Install it from the Muxflow desktop app (Settings → Connection).",
    });
  });

  it("disconnect() while the dial is still pending aborts that dial", async () => {
    const h = harness();
    let signal: AbortSignal | undefined;
    const store = createSessionStore();
    const connection = new HostConnection({
      dial: (s) =>
        new Promise<never>((_resolve, reject) => {
          signal = s;
          s.addEventListener("abort", () => reject(new Error("aborted")));
        }),
      nextConnectionEpoch: () => (h.epoch += 1),
      store,
      host: { id: "h", label: "Dev box", host: "dev.local", port: 22, user: "dev" },
      terminals: { seed: () => undefined, output: () => undefined },
      log: () => undefined,
    });
    connection.connect();
    await settle();
    expect(signal?.aborted).toBe(false);
    connection.disconnect();
    expect(signal?.aborted).toBe(true);
    await settle();
    expect(store.getState().connection.state).toBe("idle");
  });

  it("disconnect() goes idle, rejects in-flight requests, and never reconnects", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    const pending = h.connection.request(terminalInput("%1", new Uint8Array([1])));
    h.connection.disconnect();
    await expect(pending).rejects.toThrow("disconnected");
    expect(transport.closed).toBe(true);
    expect(h.store.getState().connection.state).toBe("idle");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(h.dials).toBe(1);
  });

  it("reconnects when the handshake gets no answer within 20 s", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    await vi.advanceTimersByTimeAsync(19_999);
    expect(h.store.getState().connection.state).toBe("handshaking");
    await vi.advanceTimersByTimeAsync(1);
    expect(h.store.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1, message: "handshake timed out" });
    expect(h.transports[0]!.closed).toBe(true);
  });

  it("acknowledges the charge of events dropped at the snapshot barrier", async () => {
    const h = harness();
    h.connection.connect();
    await settle();
    const transport = h.transports[0]!;
    transport.feed(hostEnvelope({ case: "serverHello", value: serverHello() }, { requestId: 1n }));
    transport.feed(event(EventKind.PANE_RESOURCE, 1n, { terminalDeliveryBytes: 0n, terminalDeliveryRecords: 1n }));
    transport.drain();
    transport.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot(), acceptedSequence: 1n }) }, { requestId: 2n }));
    await vi.advanceTimersByTimeAsync(50);
    const [ack] = transport.drain();
    if (ack?.payload.case !== "terminalOutputAck") throw new Error("expected an ack for the dropped event");
    expect(ack.payload.value).toMatchObject({ cumulativeBytes: 0n, cumulativeRecords: 1n });
  });

  it("a non-retryable connection-level error frame toasts and reconnects", async () => {
    const h = harness();
    const transport = await connectHappily(h);
    transport.feed(hostEnvelope({ case: "error", value: { $typeName: "tmux_agent.protocol.v1.Error", code: "x", displayMessage: "tmux died", retryable: false } }));
    expect(h.store.getState().connection.state).toBe("reconnecting");
  });
});
