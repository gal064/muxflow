import { describe, expect, it, vi } from "vitest";

import {
  createMuxflowSsh,
  NATIVE_EVENT_NAME,
  NATIVE_WAKE_EVENT_NAME,
  WRITE_CHUNK_BASE64_CHARS,
  type NativeMuxflowSshModule,
  type SshCloseReason,
  type SshEvent,
} from "./MuxflowSsh";

interface Harness {
  native: NativeMuxflowSshModule;
  /** Pushes a raw payload the way the Kotlin module would. */
  emit: (payload: unknown) => void;
  /** Pushes a raw `onWake` payload. */
  emitWake: (payload: unknown) => void;
  listenerCount: () => number;
  removeCalls: () => number;
  writes: Array<[string, string]>;
  resolveWrite: () => void;
}

function harness(options: { deferWrites?: boolean } = {}): Harness {
  const nativeListeners = new Set<(payload: unknown) => void>();
  const wakeListeners = new Set<(payload: unknown) => void>();
  const writes: Array<[string, string]> = [];
  const pending: Array<() => void> = [];
  let removeCalls = 0;

  const native: NativeMuxflowSshModule = {
    generateKeyPair: vi.fn(async () => ({ publicKeyOpenSsh: "ssh-ed25519 AAAA muxflow-mobile" })),
    getPublicKey: vi.fn(async () => null),
    deleteKeyPair: vi.fn(async () => undefined),
    connect: vi.fn(async () => undefined),
    trustHostKey: vi.fn(async () => undefined),
    write: vi.fn(async (connectionId: string, base64: string) => {
      writes.push([connectionId, base64]);
      if (options.deferWrites) {
        await new Promise<void>((resolve) => pending.push(resolve));
      }
    }),
    close: vi.fn(async () => undefined),
    startForegroundService: vi.fn(async () => undefined),
    stopForegroundService: vi.fn(async () => undefined),
    scheduleWake: vi.fn(async () => undefined),
    cancelWake: vi.fn(async () => undefined),
    addListener: vi.fn((eventName, listener) => {
      const set = eventName === NATIVE_WAKE_EVENT_NAME ? wakeListeners : nativeListeners;
      expect([NATIVE_EVENT_NAME, NATIVE_WAKE_EVENT_NAME]).toContain(eventName);
      set.add(listener);
      return {
        remove: () => {
          removeCalls += 1;
          set.delete(listener);
        },
      };
    }),
  };

  return {
    native,
    emit: (payload) => {
      for (const listener of [...nativeListeners]) {
        listener(payload);
      }
    },
    emitWake: (payload) => {
      for (const listener of [...wakeListeners]) {
        listener(payload);
      }
    },
    listenerCount: () => nativeListeners.size,
    removeCalls: () => removeCalls,
    writes,
    resolveWrite: () => pending.shift()?.(),
  };
}

const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

function collect(h: Harness): { events: SshEvent[]; unsubscribe: () => void } {
  const events: SshEvent[] = [];
  const ssh = createMuxflowSsh(h.native);
  const unsubscribe = ssh.addListener((event) => events.push(event));
  return { events, unsubscribe };
}

describe("event mapping", () => {
  it("maps each native payload onto the typed union", () => {
    const h = harness();
    const { events } = collect(h);

    h.emit({
      type: "hostKey",
      connectionId: "c1",
      algorithm: "ssh-ed25519",
      fingerprintSha256: "SHA256:abc",
    });
    h.emit({ type: "connected", connectionId: "c1" });
    h.emit({ type: "data", connectionId: "c1", base64: "aGk=" });
    h.emit({ type: "stderr", connectionId: "c1", text: "bash: no" });
    h.emit({ type: "closed", connectionId: "c1", exitCode: 0, reason: "exited" });

    expect(events).toEqual([
      {
        type: "hostKey",
        connectionId: "c1",
        algorithm: "ssh-ed25519",
        fingerprintSha256: "SHA256:abc",
      },
      { type: "connected", connectionId: "c1" },
      { type: "data", connectionId: "c1", base64: "aGk=" },
      { type: "stderr", connectionId: "c1", text: "bash: no" },
      { type: "closed", connectionId: "c1", exitCode: 0, reason: "exited" },
    ]);
  });

  it("drops payloads it cannot make sense of", () => {
    const h = harness();
    const { events } = collect(h);

    h.emit(null);
    h.emit("connected");
    h.emit({ type: "connected" });
    h.emit({ type: "somethingNew", connectionId: "c1" });
    h.emit({ type: "data", connectionId: "c1" });
    h.emit({ type: "hostKey", connectionId: "c1", algorithm: "ssh-ed25519" });
    h.emit({ type: "stderr", connectionId: "c1", text: 7 });

    expect(events).toEqual([]);
  });

  it("subscribes to the native module once and unsubscribes with the last listener", () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    const first: SshEvent[] = [];
    const second: SshEvent[] = [];

    const removeFirst = ssh.addListener((event) => first.push(event));
    const removeSecond = ssh.addListener((event) => second.push(event));
    expect(h.native.addListener).toHaveBeenCalledTimes(1);
    expect(h.listenerCount()).toBe(1);

    h.emit({ type: "connected", connectionId: "c1" });
    expect(first).toHaveLength(1);
    expect(second).toHaveLength(1);

    removeFirst();
    removeFirst(); // idempotent
    expect(h.removeCalls()).toBe(0);

    h.emit({ type: "connected", connectionId: "c1" });
    expect(first).toHaveLength(1);
    expect(second).toHaveLength(2);

    removeSecond();
    expect(h.removeCalls()).toBe(1);
    expect(h.listenerCount()).toBe(0);
  });
});

describe("wake timer", () => {
  it("forwards scheduleWake and cancelWake, and delivers the token of an onWake event", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    const tokens: string[] = [];
    const unsubscribe = ssh.addWakeListener((token) => tokens.push(token));

    await ssh.scheduleWake("7", 1000);
    await ssh.cancelWake("7");
    expect(h.native.scheduleWake).toHaveBeenCalledWith("7", 1000);
    expect(h.native.cancelWake).toHaveBeenCalledWith("7");

    h.emitWake({ token: "7" });
    h.emitWake({ token: 7 });
    h.emitWake(null);
    expect(tokens).toEqual(["7"]);

    // Its own native subscription: dropping the last wake listener leaves ssh listeners alone.
    const removeSsh = ssh.addListener(() => undefined);
    unsubscribe();
    expect(h.removeCalls()).toBe(1);
    expect(h.listenerCount()).toBe(1);
    h.emitWake({ token: "8" });
    expect(tokens).toEqual(["7"]);
    removeSsh();
  });
});

describe("close reasons", () => {
  const reasons: SshCloseReason[] = [
    "hostKeyNotTrusted",
    "hostKeyMismatch",
    "authFailed",
    "connectFailed",
    "exited",
    "closedByClient",
    "networkLost",
  ];

  it.each(reasons)("passes %s through unchanged", (reason) => {
    const h = harness();
    const { events } = collect(h);
    h.emit({ type: "closed", connectionId: "c1", exitCode: null, reason });
    expect(events).toEqual([{ type: "closed", connectionId: "c1", exitCode: null, reason }]);
  });

  it("falls back to the retryable reason when the native side reports something unknown", () => {
    const h = harness();
    const { events } = collect(h);
    h.emit({ type: "closed", connectionId: "c1", exitCode: null, reason: "kaboom" });
    h.emit({ type: "closed", connectionId: "c1", exitCode: null });
    expect(events.map((event) => event.type === "closed" && event.reason)).toEqual([
      "networkLost",
      "networkLost",
    ]);
  });

  it("normalises a missing exit code to null and keeps 127", () => {
    const h = harness();
    const { events } = collect(h);
    h.emit({ type: "closed", connectionId: "c1", reason: "exited" });
    h.emit({ type: "closed", connectionId: "c1", exitCode: 127, reason: "exited" });
    expect(events.map((event) => event.type === "closed" && event.exitCode)).toEqual([null, 127]);
  });
});

describe("write", () => {
  it("sends a small payload as one native write", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    await ssh.write("c1", "aGVsbG8=");
    expect(h.writes).toEqual([["c1", "aGVsbG8="]]);
  });

  it("sends nothing for an empty payload", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    await ssh.write("c1", "");
    expect(h.writes).toEqual([]);
  });

  it("splits a large payload on 4-character boundaries without losing bytes", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    const payload = "A".repeat(WRITE_CHUNK_BASE64_CHARS * 2 + 8); // two full chunks and a tail
    await ssh.write("c1", payload);

    expect(h.writes.length).toBe(3);
    expect(h.writes.map(([, chunk]) => chunk).join("")).toBe(payload);
    for (const [connectionId, chunk] of h.writes) {
      expect(connectionId).toBe("c1");
      expect(chunk.length % 4).toBe(0);
      expect(chunk.length).toBeLessThanOrEqual(WRITE_CHUNK_BASE64_CHARS);
    }
  });

  it("keeps one write in flight per connection", async () => {
    const h = harness({ deferWrites: true });
    const ssh = createMuxflowSsh(h.native);

    const first = ssh.write("c1", "AAAA");
    const second = ssh.write("c1", "BBBB");
    await flush();
    expect(h.writes).toEqual([["c1", "AAAA"]]);

    h.resolveWrite();
    await first;
    await flush();
    expect(h.writes).toEqual([
      ["c1", "AAAA"],
      ["c1", "BBBB"],
    ]);

    h.resolveWrite();
    await second;
  });

  it("keeps the queue usable after a failed write", async () => {
    const h = harness();
    (h.native.write as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error("broken pipe"));
    const ssh = createMuxflowSsh(h.native);

    await expect(ssh.write("c1", "AAAA")).rejects.toThrow("broken pipe");
    await expect(ssh.write("c1", "BBBB")).resolves.toBeUndefined();
    expect(h.writes).toEqual([["c1", "BBBB"]]);
  });
});

describe("pass-through", () => {
  it("forwards the remaining calls to the native module", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);
    const target = { host: "example.test", port: 2222, user: "dev" };

    await ssh.generateKeyPair();
    await ssh.getPublicKey();
    await ssh.deleteKeyPair();
    await ssh.connect("c1", target, "muxflow-host bridge --stdio", null);
    await ssh.trustHostKey("c1", "SHA256:abc");
    await ssh.close("c1");
    await ssh.startForegroundService("Connected to example.test", "1 agent");
    await ssh.stopForegroundService();

    expect(h.native.connect).toHaveBeenCalledWith(
      "c1",
      target,
      "muxflow-host bridge --stdio",
      null,
    );
    expect(h.native.trustHostKey).toHaveBeenCalledWith("c1", "SHA256:abc");
    expect(h.native.close).toHaveBeenCalledWith("c1");
    expect(h.native.startForegroundService).toHaveBeenCalledWith(
      "Connected to example.test",
      "1 agent",
    );
    expect(h.native.stopForegroundService).toHaveBeenCalledTimes(1);
  });
});

describe("two channels on one host", () => {
  it("routes events to listeners keyed by connectionId", () => {
    const h = harness();
    const { events } = collect(h);

    // The bridge connection and the bulk connection share one transport natively, but each
    // channel reports under its own id.
    h.emit({ type: "connected", connectionId: "bridge" });
    h.emit({ type: "connected", connectionId: "bulk" });
    h.emit({ type: "data", connectionId: "bulk", base64: "Ym9keQ==" });
    h.emit({ type: "closed", connectionId: "bulk", exitCode: 0, reason: "closedByClient" });
    h.emit({ type: "data", connectionId: "bridge", base64: "ZnJhbWU=" });

    expect(events).toEqual([
      { type: "connected", connectionId: "bridge" },
      { type: "connected", connectionId: "bulk" },
      { type: "data", connectionId: "bulk", base64: "Ym9keQ==" },
      { type: "closed", connectionId: "bulk", exitCode: 0, reason: "closedByClient" },
      { type: "data", connectionId: "bridge", base64: "ZnJhbWU=" },
    ]);
  });

  it("queues writes per connection, so a stalled channel cannot block its sibling", async () => {
    const h = harness({ deferWrites: true });
    const ssh = createMuxflowSsh(h.native);

    const bridgeFirst = ssh.write("bridge", "AAAA");
    ssh.write("bridge", "BBBB");
    const bulk = ssh.write("bulk", "CCCC");
    await flush();

    // Both connections got their first write out; only the second write on "bridge" is waiting.
    expect(h.writes).toEqual([
      ["bridge", "AAAA"],
      ["bulk", "CCCC"],
    ]);

    h.resolveWrite();
    await bridgeFirst;
    h.resolveWrite();
    await bulk;
    await flush();
    expect(h.writes.map(([id]) => id)).toEqual(["bridge", "bulk", "bridge"]);
  });

  it("keeps a write issued after a close behind the one still in flight", async () => {
    const h = harness({ deferWrites: true });
    const ssh = createMuxflowSsh(h.native);

    const inFlight = ssh.write("bridge", "AAAA");
    await flush();
    void ssh.close("bridge");
    const afterClose = ssh.write("bridge", "BBBB");
    await flush();
    expect(h.writes).toEqual([["bridge", "AAAA"]]);

    h.resolveWrite();
    await inFlight;
    await flush();
    h.resolveWrite();
    await afterClose;
    expect(h.writes).toEqual([
      ["bridge", "AAAA"],
      ["bridge", "BBBB"],
    ]);
  });

  it("closing one connection leaves the other's queue alone", async () => {
    const h = harness();
    const ssh = createMuxflowSsh(h.native);

    await ssh.close("bridge");
    await ssh.write("bulk", "CCCC");

    expect(h.native.close).toHaveBeenCalledWith("bridge");
    expect(h.writes).toEqual([["bulk", "CCCC"]]);
  });
});
