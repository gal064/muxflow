import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeTransport, hostEnvelope, okResponse, serverHello, topologySnapshot } from "../protocol/testing/fakeTransport";
import { sessionStore } from "../store/sessionStore";
import type { SavedHost } from "../store/hostsStore";
import { createMuxflowSsh, type NativeMuxflowSshModule } from "../ssh/MuxflowSsh";
import { connectHost, disconnectHost, getConnection, onToast, openBulkConnection, setForegroundService, setTransportFactory, toast } from "./connectionManager";
import { logStore } from "./log";

const host: SavedHost = { id: "h1", label: "Dev box", host: "dev.local", port: 22, user: "dev", trustedHostKeyFingerprint: null, connectionEpoch: 0, lastConnectedAtMs: null };
const settle = () => vi.advanceTimersByTimeAsync(0);

/** The Kotlin module's notification surface: records the text it was given, taps Disconnect on demand. */
function fakeNative() {
  const disconnectListeners = new Set<(payload: unknown) => void>();
  const notifications: Array<[string, string]> = [];
  const unsupported = () => Promise.reject(new Error("not part of this fake"));
  const native: NativeMuxflowSshModule = {
    generateKeyPair: unsupported,
    getPublicKey: unsupported,
    deleteKeyPair: unsupported,
    connect: unsupported,
    trustHostKey: unsupported,
    write: unsupported,
    close: unsupported,
    startForegroundService: unsupported,
    stopForegroundService: unsupported,
    setServiceNotification: vi.fn(async (title: string, body: string) => {
      notifications.push([title, body]);
    }),
    scheduleWake: unsupported,
    cancelWake: unsupported,
    addListener: vi.fn((eventName, listener) => {
      if (eventName !== "onDisconnectRequested") throw new Error(`unexpected subscription to ${eventName}`);
      disconnectListeners.add(listener);
      return { remove: () => disconnectListeners.delete(listener) };
    }),
  };
  return {
    native,
    notifications,
    tapDisconnect: () => {
      for (const listener of [...disconnectListeners]) listener({});
    },
  };
}

describe("connectionManager", () => {
  const dials: Array<{ lane: string; transport: FakeTransport }> = [];
  let fake: ReturnType<typeof fakeNative>;
  beforeEach(() => {
    vi.useFakeTimers();
    dials.length = 0;
    setTransportFactory(async (_host, lane) => {
      const transport = new FakeTransport();
      dials.push({ lane, transport });
      return transport;
    });
    fake = fakeNative();
    setForegroundService(createMuxflowSsh(fake.native));
  });
  afterEach(async () => {
    await disconnectHost();
    setForegroundService(undefined);
    vi.useRealTimers();
  });

  async function connectControl() {
    const pending = connectHost(host);
    await settle();
    const control = dials[0]!.transport;
    // The epoch is per host and monotonic across connects (§8.3), so read it back.
    const epoch = getConnection()!.connectionEpoch;
    control.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch }) }, { requestId: 1n }));
    control.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await pending;
    return { control, epoch };
  }

  it("dials the control lane, drives the store, and resolves on connected", async () => {
    const { epoch } = await connectControl();
    expect(dials.map((d) => d.lane)).toEqual(["control"]);
    expect(sessionStore.getState().connection).toMatchObject({ state: "connected", host: { label: "Dev box" } });
    expect(getConnection()?.connectionEpoch).toBe(epoch);
    expect(epoch).toBeGreaterThanOrEqual(1n);
  });

  it("shows host-provided toast text without copying that content into diagnostics", () => {
    const shown: string[] = [];
    const unsubscribe = onToast((message) => shown.push(message));
    logStore.getState().clear();
    toast("prompt=private host content");
    unsubscribe();
    expect(shown).toEqual(["prompt=private host content"]);
    expect(logStore.getState().lines.at(-1)).toContain("toast shown chars=27");
    expect(logStore.getState().lines.join("\n")).not.toContain("private host content");
  });

  it("opens the bulk lane bound to the control epoch, without Subscribe, and memoises it", async () => {
    const { epoch } = await connectControl();
    const pending = openBulkConnection();
    await settle();
    const bulk = dials[1]!;
    expect(bulk.lane).toBe("bulk");
    const [hello, ...rest] = bulk.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value).toMatchObject({ bulkConnection: true, expectedServerIdentity: "server-a", connectionEpoch: epoch });
    expect(rest).toHaveLength(0);
    bulk.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch, terminalOutputWindowBytes: 0n }) }, { requestId: 1n }));
    const lane = await pending;
    expect(lane.state).toBe("connected");
    expect(await openBulkConnection()).toBe(lane);
    expect(dials).toHaveLength(2);
    // The control store is untouched by the bulk lane.
    expect(sessionStore.getState().connection.state).toBe("connected");
  });

  it("re-dials the bulk lane after the control connection reconnects", async () => {
    const { control, epoch } = await connectControl();
    const first = openBulkConnection();
    await settle();
    dials[1]!.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch }) }, { requestId: 1n }));
    await first;
    control.closeFromRemote({ reason: "networkLost" });
    await vi.advanceTimersByTimeAsync(1_000);
    const control2 = dials[2]!.transport;
    const epoch2 = getConnection()!.connectionEpoch;
    expect(epoch2).toBe(epoch + 1n);
    control2.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch2 }) }, { requestId: 1n }));
    control2.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    expect(dials[1]!.transport.closed).toBe(true);
    const second = openBulkConnection();
    await settle();
    expect(dials).toHaveLength(4);
    const [hello] = dials[3]!.transport.drain();
    if (hello?.payload.case !== "clientHello") throw new Error("expected ClientHello");
    expect(hello.payload.value.connectionEpoch).toBe(epoch2);
    dials[3]!.transport.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch2 }) }, { requestId: 1n }));
    await second;
  });

  it("treats Disconnect on the notification as the user disconnecting, even between attempts", async () => {
    const { control } = await connectControl();
    control.closeFromRemote({ reason: "networkLost" });
    expect(sessionStore.getState().connection).toMatchObject({ state: "reconnecting", attempt: 1 });
    // The tap lands while the backoff is pending: no channel is open to report `closedByClient`.
    fake.tapDisconnect();
    await settle();
    expect(sessionStore.getState().connection.state).toBe("idle");
    expect(getConnection()).toBeNull();
    // The pending backoff went with it: nothing dials, however long the clock runs.
    await vi.advanceTimersByTimeAsync(120_000);
    expect(dials).toHaveLength(1);
  });

  it("keeps the notification text on the control connection's state", async () => {
    const { control, epoch } = await connectControl();
    await settle();
    // Once before the dial, for the automatic service start on `connected`, and once on `connected` itself.
    expect(fake.notifications).toEqual([
      ["Muxflow", "Connected to Dev box"],
      ["Muxflow", "Connected to Dev box"],
    ]);
    control.closeFromRemote({ reason: "networkLost" });
    await settle();
    expect(fake.notifications.at(-1)).toEqual(["Muxflow", "Reconnecting to Dev box"]);
    await vi.advanceTimersByTimeAsync(1_000);
    // The re-dial itself (`sshConnecting`, `handshaking`) posts nothing.
    expect(fake.notifications).toHaveLength(3);
    const control2 = dials[1]!.transport;
    const epoch2 = getConnection()!.connectionEpoch;
    expect(epoch2).toBe(epoch + 1n);
    control2.feed(hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: epoch2 }) }, { requestId: 1n }));
    control2.feed(hostEnvelope({ case: "response", value: okResponse({ snapshot: topologySnapshot() }) }, { requestId: 2n }));
    await settle();
    expect(fake.notifications.at(-1)).toEqual(["Muxflow", "Connected to Dev box"]);
    expect(fake.notifications).toHaveLength(4);
  });

  it("rejects connectHost on a fatal close and disconnectHost returns the store to idle", async () => {
    const pending = connectHost(host);
    await settle();
    dials[0]!.transport.closeFromRemote({ reason: "authFailed" });
    await expect(pending).rejects.toThrow(/rejected this phone's SSH login/);
    await disconnectHost();
    expect(sessionStore.getState().connection.state).toBe("idle");
    expect(getConnection()).toBeNull();
  });
});
