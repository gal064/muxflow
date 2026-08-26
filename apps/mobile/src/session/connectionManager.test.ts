import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../protocol/testing/fakeTransport";
import { sessionStore } from "../store/sessionStore";
import type { SavedHost } from "../store/hostsStore";
import { connectHost, disconnectHost, getConnection, openBulkConnection, setTransportFactory } from "./connectionManager";

const host: SavedHost = { id: "h1", label: "Dev box", host: "dev.local", port: 22, user: "dev", trustedHostKeyFingerprint: null, connectionEpoch: 0, lastConnectedAtMs: null };
const settle = () => vi.advanceTimersByTimeAsync(0);

describe("connectionManager", () => {
  const dials: Array<{ lane: string; transport: FakeTransport }> = [];
  beforeEach(() => {
    vi.useFakeTimers();
    dials.length = 0;
    setTransportFactory(async (_host, lane) => {
      const transport = new FakeTransport();
      dials.push({ lane, transport });
      return transport;
    });
  });
  afterEach(async () => {
    await disconnectHost();
    vi.useRealTimers();
  });

  async function connectControl() {
    const pending = connectHost(host);
    await settle();
    const control = dials[0]!.transport;
    control.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 1n }) }, { requestId: 1n }));
    control.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await pending;
    return control;
  }

  it("dials the control lane, drives the store, and resolves on connected", async () => {
    await connectControl();
    expect(dials.map((d) => d.lane)).toEqual(["control"]);
    expect(sessionStore.getState().connection).toMatchObject({ state: "connected", host: { label: "Dev box" } });
    expect(getConnection()?.connectionEpoch).toBe(1n);
  });

  it("opens the bulk lane bound to the control epoch, without Subscribe, and memoises it", async () => {
    await connectControl();
    const pending = openBulkConnection();
    await settle();
    const bulk = dials[1]!;
    expect(bulk.lane).toBe("bulk");
    const [hello, ...rest] = bulk.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value).toMatchObject({ bulkConnection: true, expectedServerIdentity: "server-a", connectionEpoch: 1n });
    expect(rest).toHaveLength(0);
    bulk.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 1n, terminalOutputWindowBytes: 0n }) }, { requestId: 1n }));
    const lane = await pending;
    expect(lane.state).toBe("connected");
    expect(await openBulkConnection()).toBe(lane);
    expect(dials).toHaveLength(2);
    // The control store is untouched by the bulk lane.
    expect(sessionStore.getState().connection.state).toBe("connected");
  });

  it("re-dials the bulk lane after the control connection reconnects", async () => {
    const control = await connectControl();
    const first = openBulkConnection();
    await settle();
    dials[1]!.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 1n }) }, { requestId: 1n }));
    await first;
    control.closeFromRemote({ reason: "networkLost" });
    await vi.advanceTimersByTimeAsync(1_000);
    const control2 = dials[2]!.transport;
    control2.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 2n }) }, { requestId: 1n }));
    control2.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    expect(dials[1]!.transport.closed).toBe(true);
    const second = openBulkConnection();
    await settle();
    expect(dials).toHaveLength(4);
    const [hello] = dials[3]!.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value.connectionEpoch).toBe(2n);
    dials[3]!.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 2n }) }, { requestId: 1n }));
    await second;
  });

  it("rejects connectHost on a fatal close and disconnectHost returns the store to idle", async () => {
    const pending = connectHost(host);
    await settle();
    dials[0]!.transport.closeFromRemote({ reason: "authFailed" });
    await expect(pending).rejects.toThrow(/rejected this phone's SSH key/);
    await disconnectHost();
    expect(sessionStore.getState().connection.state).toBe("idle");
    expect(getConnection()).toBeNull();
  });
});
