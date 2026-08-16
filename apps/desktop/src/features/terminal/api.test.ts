import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, perfCounterSnapshot, resetPerfProbe } from "../../perf/probe";
import {
  decodeTerminalEvent,
  FINAL_BRIDGE_DELIVERY_WAIT_MS,
  FINAL_BRIDGE_SHUTDOWN_WAIT_MS,
  MAX_HOST_TERMINAL_INPUT_BYTES,
  prepareTerminalSnapshot,
  requestTerminalSeed,
  sendBinaryInput,
  sendInput,
  setTerminalVisibility,
  startTerminal,
  stopTerminal,
  terminalBridgeKey,
  terminalBridgeScope,
  type TerminalEvent,
} from "./api";

const channels = vi.hoisted(() => [] as Array<{ onmessage?: (message: ArrayBuffer) => void }>);

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class MockChannel<T> {
    onmessage?: (message: T) => void;
    constructor() { channels.push(this as { onmessage?: (message: ArrayBuffer) => void }); }
  },
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("../../perf/bootstrap", () => ({ perfProbeReady: () => Promise.resolve(true) }));

const textEncoder = new TextEncoder();

function u64(value: number | bigint): Uint8Array {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt(value), false);
  return bytes;
}

function u32(value: number): Uint8Array {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value, false);
  return bytes;
}

function frame(kind: number, label: string, sequence: number, payload: Uint8Array<ArrayBufferLike> = new Uint8Array()): ArrayBuffer {
  const encodedLabel = textEncoder.encode(label);
  return Uint8Array.from([
    kind, encodedLabel.length >> 8, encodedLabel.length & 0xff, ...encodedLabel,
    ...u64(sequence), ...payload,
  ]).buffer;
}

function paneResourcePayload(options: {
  state?: number;
  flags?: number;
  generation?: number;
  snapshotGeneration?: number;
  tailThroughGeneration?: number;
  reason?: Uint8Array<ArrayBufferLike>;
  snapshot?: Uint8Array<ArrayBufferLike>;
  tail?: Uint8Array<ArrayBufferLike>;
} = {}): Uint8Array {
  const reason = options.reason ?? new Uint8Array();
  const snapshot = options.snapshot ?? new Uint8Array();
  const tail = options.tail ?? new Uint8Array();
  return Uint8Array.from([
    options.state ?? 2, options.flags ?? 0, ...u64(options.generation ?? 19),
    ...u64(options.snapshotGeneration ?? 17), ...u64(options.tailThroughGeneration ?? 19),
    ...u32(reason.byteLength), ...u32(snapshot.byteLength), ...u32(tail.byteLength),
    ...reason, ...snapshot, ...tail,
  ]);
}

beforeEach(() => {
  resetPerfProbe();
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue(undefined);
  channels.length = 0;
});

describe("binary terminal IPC", () => {
  it("decodes the common sequence and arbitrary output bytes without JSON byte arrays", () => {
    expect(decodeTerminalEvent(frame(2, "%4", 12, Uint8Array.from([...u64(7), 0, 255, 27])))).toEqual({
      kind: "output", paneId: "%4", sequence: 12, generation: 7, data: Uint8Array.from([0, 255, 27]),
    });
  });

  it("retains output in a compact owned allocation independent of its transport frame", () => {
    const wire = frame(2, "%4", 12, Uint8Array.from([...u64(7), 0, 255, 27]));
    const event = decodeTerminalEvent(wire);
    expect(event.kind).toBe("output");
    if (event.kind !== "output") throw new Error("expected output fixture");
    expect(event.data.byteOffset).toBe(0);
    expect(event.data.buffer.byteLength).toBe(3);
    expect(event.data.buffer).not.toBe(wire);
    new Uint8Array(wire).fill(9);
    expect([...event.data]).toEqual([0, 255, 27]);

    const empty = decodeTerminalEvent(frame(2, "%4", 13, u64(8)));
    expect(empty.kind).toBe("output");
    if (empty.kind !== "output") throw new Error("expected empty output fixture");
    expect(empty.data.buffer.byteLength).toBe(0);
  });

  it("rejects frames truncated before the label, sequence, or terminal generation", () => {
    expect(() => decodeTerminalEvent(Uint8Array.from([1, 0, 4, 37]).buffer)).toThrow("common header");
    expect(() => decodeTerminalEvent(Uint8Array.from([1, 0, 4, 37, 49, 50, 51, ...u64(1).slice(0, 7)]).buffer)).toThrow("truncated");
    expect(() => decodeTerminalEvent(frame(1, "%1", 1, new Uint8Array(7)))).toThrow("generation");
  });

  it("accepts connection/error/epoch only as local sequence-zero frames", () => {
    expect(decodeTerminalEvent(frame(6, "disconnected", 0))).toEqual({
      kind: "connectionState", state: "disconnected", sequence: 0,
    });
    expect(() => decodeTerminalEvent(frame(6, "connected", 1))).toThrow("sequence zero");
    expect(() => decodeTerminalEvent(frame(4, "broken", 1))).toThrow("sequence zero");
    expect(() => decodeTerminalEvent(frame(10, "terminal", 1, u64(7)))).toThrow("sequence zero");
  });

  it("preserves authoritative snapshot metadata and rejects split sequence metadata", () => {
    const payload = textEncoder.encode(JSON.stringify({
      snapshot: { sessions: [], windows: [], panes: [] }, sequence: 12, generation: 7,
      serverIdentity: "tmux:test", authoritative: true,
    }));
    expect(decodeTerminalEvent(frame(7, "snapshot", 12, payload))).toMatchObject({
      kind: "snapshot", sequence: 12, generation: 7, serverIdentity: "tmux:test", authoritative: true,
    });
    expect(() => decodeTerminalEvent(frame(7, "snapshot", 13, payload))).toThrow("conflicts");
  });

  it("decodes kind 8 only as standalone protocol progress", () => {
    expect(decodeTerminalEvent(frame(8, "protocol", 9))).toEqual({ kind: "protocolProgress", sequence: 9 });
    expect(() => decodeTerminalEvent(frame(8, "9", 9))).toThrow("malformed");
    expect(() => decodeTerminalEvent(frame(8, "protocol", 9, Uint8Array.of(1)))).toThrow("malformed");
  });

  it("decodes ordered file-service events without losing opaque u64 strings", () => {
    const event = { operationId: "op", activeRoot: undefined, directory: undefined, metadata: { path: "/r/a", generation: "18446744073709551615" }, transferId: "", transferredBytes: "0", totalBytes: "0", state: "", error: "" };
    expect(decodeTerminalEvent(frame(12, "/r/a", 17, textEncoder.encode(JSON.stringify(event))))).toEqual({
      kind: "fileService", scope: "/r/a", sequence: 17, event,
    });
    expect(() => decodeTerminalEvent(frame(12, "x", 0, textEncoder.encode(JSON.stringify(event))))).toThrow("nonzero");
    expect(() => decodeTerminalEvent(frame(12, "x", 1, Uint8Array.of(0xff)))).toThrow("UTF-8 JSON");
  });

  it("decodes ordered Git-service status while retaining raw path byte arrays", () => {
    const event = { watchId: "watch", rootToken: "root", status: { generation: "18446744073709551615", entries: [{ path: [45, 45, 0, 10] }] }, error: "" };
    expect(decodeTerminalEvent(frame(13, "/repo", 18, textEncoder.encode(JSON.stringify(event))))).toEqual({
      kind: "gitService", scope: "/repo", sequence: 18, event,
    });
    expect(() => decodeTerminalEvent(frame(13, "/repo", 0, textEncoder.encode(JSON.stringify(event))))).toThrow("nonzero");
    expect(() => decodeTerminalEvent(frame(13, "/repo", 1, Uint8Array.of(0xff)))).toThrow("UTF-8 JSON");
  });

  it("decodes ordered agent events and authoritative reconnect snapshots losslessly", () => {
    const event = { generation: "18446744073709551615", connectionEpoch: "41", notify: true, reason: "blocked", agent: { agentId: "codex:1" } };
    expect(decodeTerminalEvent(frame(14, "agent:codex:1", 0, textEncoder.encode(JSON.stringify(event))))).toEqual({
      kind: "agentService", scope: "agent:codex:1", sequence: 0, event,
    });
    const snapshot = { generation: "20", acceptedGeneration: "20", agents: [], authoritative: true, notificationWatermark: "20", connectionEpoch: "41" };
    expect(decodeTerminalEvent(frame(14, "snapshot", 0, textEncoder.encode(JSON.stringify(snapshot))))).toEqual({
      kind: "agentService", scope: "snapshot", sequence: 0, snapshot,
    });
    expect(decodeTerminalEvent(frame(14, "agent", 0, textEncoder.encode(JSON.stringify(event)))).sequence).toBe(0);
    expect(() => decodeTerminalEvent(frame(14, "agent", 2, textEncoder.encode(JSON.stringify(event))))).toThrow("agent service frame must use local sequence zero");
    expect(() => decodeTerminalEvent(frame(14, "agent", 0, Uint8Array.of(0xff)))).toThrow("UTF-8 JSON");
  });

  it("decodes compact pane recovery material with strict length-delimited byte segments", () => {
    const payload = paneResourcePayload({
      flags: 1,
      reason: textEncoder.encode("overflow λ"),
      snapshot: Uint8Array.from([27, 91, 109]),
      tail: Uint8Array.from([255, 0]),
    });
    expect(decodeTerminalEvent(frame(9, "%7", 20, payload))).toEqual({
      kind: "paneResource", paneId: "%7", state: "hiddenBuffered", requiresSeed: true,
      recoveryReason: "overflow λ", generation: 19, snapshotGeneration: 17, tailThroughGeneration: 19,
      serializedSnapshot: Uint8Array.from([27, 91, 109]),
      rawTail: Uint8Array.from([255, 0]), sequence: 20,
    });
    expect(payload.byteLength).toBe(54);
  });

  it("owns compact recovery segments without retaining their full transport frame", () => {
    const wire = frame(9, "%7", 20, paneResourcePayload({
      reason: textEncoder.encode("overflow"), snapshot: Uint8Array.of(1, 2, 3), tail: Uint8Array.of(4, 5),
    }));
    const event = decodeTerminalEvent(wire);
    expect(event.kind).toBe("paneResource");
    if (event.kind !== "paneResource") throw new Error("expected pane resource fixture");
    expect(event.serializedSnapshot.buffer.byteLength).toBe(3);
    expect(event.rawTail.buffer.byteLength).toBe(2);
    expect(event.serializedSnapshot.buffer).not.toBe(wire);
    expect(event.rawTail.buffer).not.toBe(wire);
    new Uint8Array(wire).fill(9);
    expect([...event.serializedSnapshot, ...event.rawTail]).toEqual([1, 2, 3, 4, 5]);
  });

  it("rejects compact pane recovery truncation, unknown flags, invalid state, and malformed UTF-8", () => {
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, new Uint8Array(37)))).toThrow("truncated");
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, paneResourcePayload({ flags: 2 })))).toThrow("flags");
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, paneResourcePayload({ state: 4 })))).toThrow("state");
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, paneResourcePayload({ reason: Uint8Array.of(0xff) })))).toThrow("UTF-8");
    const lengthMismatch = paneResourcePayload({ snapshot: Uint8Array.of(1) }).slice(0, -1);
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, lengthMismatch))).toThrow("length fields");
    expect(() => decodeTerminalEvent(frame(9, "%7", 1, paneResourcePayload({
      snapshotGeneration: 20, tailThroughGeneration: 19,
    })))).toThrow("generation metadata");
  });

  it("encodes serialized renderer snapshots within the host cap", () => {
    const prepared = prepareTerminalSnapshot("shell: λ", 32);
    expect(new TextDecoder().decode(prepared.data)).toBe("shell: λ");
    expect(prepared).toMatchObject({ retained: true, originalByteLength: 9 });
    expect(prepareTerminalSnapshot("λλ", 3)).toMatchObject({ retained: false, originalByteLength: 4 });
  });

  it("sends pane visibility with the exact epoch, rendered cutoff, and serialized bytes", async () => {
    await setTerminalVisibility(
      "client-1", "%7", false, Uint8Array.from([0, 255, 27]),
      { terminalEpoch: 17, outputGeneration: 42 },
    );
    // One raw framed body, not a JSON array of numbers: a hide carries up to
    // 4 MiB of serialized screen on the thread that has to paint the new tab.
    const [command, payload] = vi.mocked(invoke).mock.calls.at(-1)!;
    expect(command).toBe("set_terminal_visibility");
    const frame = payload as unknown as Uint8Array;
    expect(frame).toBeInstanceOf(Uint8Array);
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    expect(new TextDecoder().decode(frame.subarray(2, 10))).toBe("client-1");
    expect(new TextDecoder().decode(frame.subarray(12, 14))).toBe("%7");
    expect(frame[14]).toBe(0);
    expect(view.getBigUint64(15, false)).toBe(17n);
    expect(view.getBigUint64(23, false)).toBe(42n);
    expect([...frame.subarray(31)]).toEqual([0, 255, 27]);
  });

  it("requests one scoped seed for bounded or conflicting recovery", async () => {
    await requestTerminalSeed("client-1", "%7");
    expect(invoke).toHaveBeenCalledWith("request_terminal_seed", { clientId: "client-1", paneId: "%7" });
  });

  it("keeps terminal input atomic by rejecting oversized text, binary, and bracketed paste calls", async () => {
    const atLimit = "x".repeat(MAX_HOST_TERMINAL_INPUT_BYTES - 12);
    await sendInput("client-1", "%7", `\u001b[200~${atLimit}\u001b[201~`);
    await expect(sendInput("client-1", "%7", `\u001b[200~${atLimit}x\u001b[201~`)).rejects.toThrow("partial commit");
    await expect(sendInput("client-1", "%7", "λ".repeat(MAX_HOST_TERMINAL_INPUT_BYTES / 2 + 1)))
      .rejects.toThrow("1 MiB");
    await expect(sendBinaryInput("client-1", "%7", new Uint8Array(MAX_HOST_TERMINAL_INPUT_BYTES + 1)))
      .rejects.toThrow("1 MiB");
    expect(invoke).toHaveBeenCalledTimes(1);
  }, 15_000);

  it("sends binary terminal input as one raw framed body, not a JSON number array", async () => {
    vi.mocked(invoke).mockClear();
    await sendBinaryInput("client-1", "%7", Uint8Array.of(0x00, 0x1b, 0xff));
    const [command, payload] = vi.mocked(invoke).mock.calls[0];
    expect(command).toBe("send_terminal_input_bytes");
    expect(payload).toBeInstanceOf(Uint8Array);
    const frame = payload as unknown as Uint8Array;
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    expect(view.getUint16(0, false)).toBe("client-1".length);
    expect(new TextDecoder().decode(frame.subarray(2, 10))).toBe("client-1");
    expect(view.getUint16(10, false)).toBe(2);
    expect(new TextDecoder().decode(frame.subarray(12, 14))).toBe("%7");
    expect([...frame.subarray(14)]).toEqual([0x00, 0x1b, 0xff]);
  });

  it("accounts exact text objects and binary frames at the Tauri boundary", async () => {
    enablePerfProbe(async () => undefined);
    const textBoundary = { clientId: "client-1", paneId: "%7", data: "λ" };
    await sendInput(textBoundary.clientId, textBoundary.paneId, textBoundary.data);
    const textBytes = new TextEncoder().encode(JSON.stringify(textBoundary)).byteLength;
    expect(perfCounterSnapshot()["terminal.hostRequestBytes"]).toBe(textBytes);

    await sendBinaryInput("client-1", "%7", Uint8Array.of(0, 255));
    const binaryFrame = vi.mocked(invoke).mock.calls.at(-1)?.[1] as unknown as Uint8Array;
    expect(perfCounterSnapshot()["terminal.hostRequestBytes"]).toBe(textBytes + binaryFrame.byteLength);
  });

  it("decodes a nonzero safe big-endian terminal generation epoch", () => {
    expect(decodeTerminalEvent(frame(10, "terminal", 0, u64(0x001f_ffff_ffff_fffen)))).toEqual({
      kind: "generationEpoch", epoch: 0x001f_ffff_ffff_fffe, sequence: 0,
    });
    expect(() => decodeTerminalEvent(frame(10, "terminal", 0, u64(0)))).toThrow("nonzero");
    expect(() => decodeTerminalEvent(frame(10, "terminal", 0, u64(BigInt(Number.MAX_SAFE_INTEGER) + 1n)))).toThrow("safe range");
  });

  it("decodes pane-scoped UTF-8 seed diagnostics and rejects malformed values", () => {
    expect(decodeTerminalEvent(frame(11, "%7", 14, textEncoder.encode("alternate metadata λ")))).toEqual({
      kind: "seedDiagnostic", paneId: "%7", message: "alternate metadata λ", sequence: 14,
    });
    expect(() => decodeTerminalEvent(frame(11, "pane-7", 1, Uint8Array.of(65)))).toThrow("pane label");
    expect(() => decodeTerminalEvent(frame(11, "%7", 1, Uint8Array.of(0xff)))).toThrow("UTF-8");
  });

  it("decodes a pane-scoped flow stall and rejects malformed values", () => {
    expect(decodeTerminalEvent(frame(15, "%7", 21, textEncoder.encode("tmux rejected the resume λ")))).toEqual({
      kind: "flowStalled", paneId: "%7", message: "tmux rejected the resume λ", sequence: 21,
    });
    expect(() => decodeTerminalEvent(frame(15, "terminal", 1, Uint8Array.of(65)))).toThrow("pane label");
    expect(() => decodeTerminalEvent(frame(15, "%7", 1, Uint8Array.of(0xff)))).toThrow("UTF-8");
  });

  it("keeps one initially-empty bridge lifecycle stable across session and topology UI changes", () => {
    const connection = { mode: "local" } as const;
    const initialKey = terminalBridgeKey(connection, 2);
    expect(terminalBridgeKey(connection, 2)).toBe(initialKey);
    expect(terminalBridgeKey(connection, 3)).not.toBe(initialKey);
    expect(terminalBridgeScope()).toEqual({ sessionId: "", paneIds: [] });
  });

  it("retries one cumulative bridge acknowledgement with the lifecycle measurement ID", async () => {
    vi.useFakeTimers();
    let acknowledgementAttempts = 0;
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-measured";
      if (command === "bridge_final_totals") return {
        cumulativeFrameCount: 1,
        cumulativeByteLength: frame(6, "connected", 0).byteLength,
        quiesced: true,
      };
      if (command === "acknowledge_bridge_events" && acknowledgementAttempts++ === 0) {
        throw new Error("transient acknowledgement failure");
      }
      return undefined;
    });
    try {
      const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);
      channels[0].onmessage?.(frame(6, "connected", 0));
      await vi.advanceTimersByTimeAsync(20);
      await stopTerminal(clientId);

      const calls = vi.mocked(invoke).mock.calls.filter(([command]) => command === "acknowledge_bridge_events");
      expect(calls).toHaveLength(2);
      expect(calls[0][1]).toEqual(calls[1][1]);
      expect(calls[0][1]).toMatchObject({
        cumulativeFrameCount: 1,
        cumulativeByteLength: frame(6, "connected", 0).byteLength,
      });
      expect((calls[0][1] as { measurementId: string }).measurementId).toMatch(
        /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps acknowledgement admission open until the stopped producer's final frame arrives", async () => {
    vi.useFakeTimers();
    const finalFrame = frame(6, "disconnected", 0);
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-final-frame";
      if (command === "stop_terminal") {
        setTimeout(() => channels[0].onmessage?.(finalFrame), 10);
        return undefined;
      }
      if (command === "bridge_final_totals") return {
        cumulativeFrameCount: 1, cumulativeByteLength: finalFrame.byteLength, quiesced: true,
      };
      return undefined;
    });
    try {
      const events: TerminalEvent[] = [];
      const clientId = await startTerminal("", [], { mode: "local" }, (event) => events.push(event));
      const stopping = stopTerminal(clientId);
      await vi.advanceTimersByTimeAsync(10);
      await stopping;

      expect(events).toEqual([{ kind: "connectionState", state: "disconnected", sequence: 0 }]);
      const calls = vi.mocked(invoke).mock.calls;
      const acknowledgementIndex = calls.findIndex(([command]) => command === "acknowledge_bridge_events");
      const finalizeIndex = calls.findIndex(([command]) => command === "finalize_bridge_measurement");
      expect(acknowledgementIndex).toBeGreaterThan(-1);
      expect(finalizeIndex).toBeGreaterThan(acknowledgementIndex);
      expect(calls[acknowledgementIndex][1]).toMatchObject({
        cumulativeFrameCount: 1,
        cumulativeByteLength: finalFrame.byteLength,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds final delivery wait and still finalizes native outstanding evidence", async () => {
    vi.useFakeTimers();
    enablePerfProbe(async () => undefined);
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "start_terminal") return "client-missing-frame";
      if (command === "bridge_final_totals") {
        return { cumulativeFrameCount: 1, cumulativeByteLength: 64, quiesced: true };
      }
      return undefined;
    });
    try {
      const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);
      const stopping = stopTerminal(clientId);
      await vi.advanceTimersByTimeAsync(FINAL_BRIDGE_DELIVERY_WAIT_MS);
      await stopping;

      const commands = vi.mocked(invoke).mock.calls.map(([command]) => command);
      expect(commands).not.toContain("acknowledge_bridge_events");
      expect(commands).toContain("finalize_bridge_measurement");
      expect(perfCounterSnapshot()).toMatchObject({
        "bridge.finalDeliveryTimeouts": 1,
        "bridge.finalDeliveryOutstandingFrames": 1,
        "bridge.finalDeliveryOutstandingBytes": 64,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds unresolved measurement acknowledgements and finalization behind ordinary stop", async () => {
    vi.useFakeTimers();
    enablePerfProbe(async () => undefined);
    const never = new Promise<never>(() => undefined);
    vi.mocked(invoke).mockImplementation((command) => {
      if (command === "start_terminal") return Promise.resolve("client-stuck-measurement");
      if (command === "stop_terminal") return Promise.resolve(undefined);
      if (command === "bridge_final_totals") {
        return Promise.resolve({ cumulativeFrameCount: 1, cumulativeByteLength: 8, quiesced: true });
      }
      if (command === "acknowledge_bridge_events" || command === "finalize_bridge_measurement") return never;
      return Promise.resolve(undefined);
    });
    try {
      const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);
      channels[0].onmessage?.(frame(6, "connected", 0));
      const stopping = stopTerminal(clientId);
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(FINAL_BRIDGE_SHUTDOWN_WAIT_MS);
      await expect(stopping).resolves.toBeUndefined();

      expect(perfCounterSnapshot()).toMatchObject({
        "bridge.measurementInvokeTimeouts": expect.any(Number),
        "bridge.finalizationIncomplete": 1,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps admission open to the absolute deadline when native quiescence never arrives", async () => {
    vi.useFakeTimers();
    enablePerfProbe(async () => undefined);
    const never = new Promise<never>(() => undefined);
    vi.mocked(invoke).mockImplementation((command) => {
      if (command === "start_terminal") return Promise.resolve("client-unquiesced");
      if (command === "stop_terminal") return Promise.resolve(undefined);
      if (command === "bridge_final_totals") return never;
      return Promise.resolve(undefined);
    });
    try {
      const clientId = await startTerminal("", [], { mode: "local" }, () => undefined);
      const stopping = stopTerminal(clientId);
      let settled = false;
      void stopping.then(() => { settled = true; });
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(FINAL_BRIDGE_SHUTDOWN_WAIT_MS / 2);
      channels[0].onmessage?.(frame(6, "disconnected", 0));
      for (let elapsed = FINAL_BRIDGE_SHUTDOWN_WAIT_MS / 2; !settled && elapsed <= FINAL_BRIDGE_SHUTDOWN_WAIT_MS; elapsed += 50) {
        await vi.advanceTimersByTimeAsync(50);
      }
      expect(settled).toBe(true);
      await expect(stopping).resolves.toBeUndefined();

      const commands = vi.mocked(invoke).mock.calls.map(([command]) => command);
      expect(commands).not.toContain("finalize_bridge_measurement");
      expect(commands).toContain("acknowledge_bridge_events");
      expect(perfCounterSnapshot()).toMatchObject({
        "bridge.finalQuiesceTimeouts": 1,
        "bridge.shutdownDeadlineTimeouts": 1,
        "bridge.finalizationIncomplete": 1,
      });
    } finally {
      vi.useRealTimers();
    }
  });
});
