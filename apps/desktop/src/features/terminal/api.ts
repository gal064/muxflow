import { Channel, invoke } from "@tauri-apps/api/core";
import { measurePerf, perfProbeEnabled, recordPerfCounter, startPerfSpan } from "../../perf/probe";
import type { ConnectionSpec, TmuxSnapshot } from "../../app/types";
import type { WireFileEvent } from "../files/api";
import type { WireGitEvent } from "../git/api";
import type { WireAgentEvent, WireAgentSnapshot } from "../agents/api";
import type { OperationRecorder } from "../../perf/operations";
import { copyTerminalBytes, ownTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";

interface SequencedTerminalEvent {
  sequence: number;
}

export type TerminalEvent = SequencedTerminalEvent & (
  | { kind: "generationEpoch"; epoch: number }
  | { kind: "seed"; paneId: string; generation: number; data: OwnedTerminalBytes }
  | { kind: "output"; paneId: string; generation: number; data: OwnedTerminalBytes }
  | { kind: "seedDiagnostic"; paneId: string; message: string }
  | { kind: "flowStalled"; paneId: string; message: string }
  | { kind: "topologyDirty"; name: string }
  | { kind: "error"; message: string }
  | { kind: "exit"; reason: string }
  | { kind: "connectionState"; state: "connecting" | "connected" | "reconnecting" | "resyncing" | "disconnected" | "readOnly" }
  | { kind: "protocolProgress" }
  | {
      kind: "paneResource";
      paneId: string;
      state: "visible" | "hiddenBuffered" | "released" | "unspecified";
      requiresSeed: boolean;
      recoveryReason: string;
      generation: number;
      snapshotGeneration: number;
      tailThroughGeneration: number;
      serializedSnapshot: OwnedTerminalBytes;
      rawTail: OwnedTerminalBytes;
    }
  | { kind: "snapshot"; snapshot: TmuxSnapshot; generation: number; serverIdentity: string; authoritative: boolean }
  | { kind: "fileService"; scope: string; event: WireFileEvent }
  | { kind: "gitService"; scope: string; event: WireGitEvent }
  | { kind: "agentService"; scope: string; event?: WireAgentEvent; snapshot?: WireAgentSnapshot }
);

const decoder = new TextDecoder("utf-8", { fatal: true });
const encoder = new TextEncoder();
export const MAX_HOST_TERMINAL_SNAPSHOT_BYTES = 4 * 1024 * 1024;
export const MAX_HOST_TERMINAL_INPUT_BYTES = 1024 * 1024;
const COMMON_HEADER_BYTES = 11;
const PANE_RESOURCE_HEADER_BYTES = 38;

declare const preparedTerminalSnapshot: unique symbol;

export interface PreparedTerminalSnapshot {
  readonly [preparedTerminalSnapshot]: true;
  serialized: string;
  data: OwnedTerminalBytes;
  originalByteLength: number;
  retained: boolean;
}

export interface TerminalVisibilityCheckpoint {
  terminalEpoch: number;
  outputGeneration: number;
}

export function prepareTerminalSnapshot(
  serialized: string,
  maxBytes = MAX_HOST_TERMINAL_SNAPSHOT_BYTES,
): PreparedTerminalSnapshot {
  const encoded = encoder.encode(serialized);
  if (encoded.byteLength > maxBytes) {
    return {
      serialized,
      data: ownTerminalBytes(new Uint8Array()),
      originalByteLength: encoded.byteLength,
      retained: false,
    } as PreparedTerminalSnapshot;
  }
  return {
    serialized,
    data: ownTerminalBytes(encoded),
    originalByteLength: encoded.byteLength,
    retained: true,
  } as PreparedTerminalSnapshot;
}

export function decodeTerminalEvent(buffer: ArrayBuffer, measurements?: OperationRecorder): TerminalEvent {
  measurements?.add("terminal.decoder.frames");
  const frame = new Uint8Array(buffer);
  if (frame.length < COMMON_HEADER_BYTES) throw new Error("terminal frame is shorter than its common header");
  const labelLength = (frame[1] << 8) | frame[2];
  const payloadOffset = COMMON_HEADER_BYTES + labelLength;
  if (frame.length < payloadOffset) throw new Error("terminal frame label or sequence is truncated");

  let label: string;
  try {
    label = decoder.decode(frame.subarray(3, 3 + labelLength));
  } catch {
    throw new Error("terminal frame label is not valid UTF-8");
  }
  const sequence = decodeSafeU64(frame.subarray(3 + labelLength, payloadOffset), "event sequence");
  const data = frame.subarray(payloadOffset);

  switch (frame[0]) {
    case 1: return decodeTerminalBytes("seed", label, sequence, data, measurements);
    case 2: return decodeTerminalBytes("output", label, sequence, data, measurements);
    case 3:
      requireHostSequence(sequence, "topology dirty");
      requireEmptyPayload(data, "topology dirty");
      return { kind: "topologyDirty", name: label, sequence };
    case 4:
      requireLocalSequence(sequence, "error");
      requireEmptyPayload(data, "error");
      return { kind: "error", message: label, sequence };
    case 5:
      requireHostSequence(sequence, "exit");
      requireEmptyPayload(data, "exit");
      return { kind: "exit", reason: label, sequence };
    case 6:
      requireLocalSequence(sequence, "connection state");
      requireEmptyPayload(data, "connection state");
      if (!["connecting", "connected", "reconnecting", "resyncing", "disconnected", "readOnly"].includes(label)) {
        throw new Error(`unknown connection state ${label}`);
      }
      return { kind: "connectionState", state: label as Extract<TerminalEvent, { kind: "connectionState" }>["state"], sequence };
    case 7: {
      if (label !== "snapshot") throw new Error("topology snapshot has an invalid label");
      let parsed: { snapshot: TmuxSnapshot; sequence: number; generation: number; serverIdentity: string; authoritative: boolean };
      try {
        parsed = JSON.parse(decoder.decode(data)) as typeof parsed;
      } catch {
        throw new Error("topology snapshot payload is not valid UTF-8 JSON");
      }
      if (!Number.isSafeInteger(parsed.sequence) || parsed.sequence < 0) throw new Error("invalid snapshot sequence");
      if (parsed.sequence !== sequence) throw new Error("snapshot sequence conflicts with its common frame header");
      if (!Number.isSafeInteger(parsed.generation) || parsed.generation < 0) throw new Error("invalid snapshot generation");
      if (typeof parsed.serverIdentity !== "string" || typeof parsed.authoritative !== "boolean" || !parsed.snapshot) {
        throw new Error("invalid topology snapshot metadata");
      }
      if (!parsed.authoritative) requireHostSequence(sequence, "ordered topology snapshot");
      const { sequence: _embeddedSequence, ...snapshot } = parsed;
      return { kind: "snapshot", sequence, ...snapshot };
    }
    case 8:
      requireHostSequence(sequence, "protocol progress");
      if (label !== "protocol" || data.byteLength !== 0) throw new Error("protocol progress frame is malformed");
      return { kind: "protocolProgress", sequence };
    case 9: return decodePaneResource(label, sequence, data, measurements);
    case 10: {
      requireLocalSequence(sequence, "terminal generation epoch");
      if (label !== "terminal") throw new Error("terminal generation epoch has an invalid label");
      const epoch = decodeSafeU64(data, "terminal generation epoch");
      if (epoch === 0) throw new Error("terminal generation epoch must be nonzero");
      return { kind: "generationEpoch", epoch, sequence };
    }
    case 11:
      requireHostSequence(sequence, "terminal seed diagnostic");
      requirePaneId(label, "terminal seed diagnostic");
      try {
        return { kind: "seedDiagnostic", paneId: label, message: decoder.decode(data), sequence };
      } catch {
        throw new Error("terminal seed diagnostic payload is not valid UTF-8");
      }
    case 12: {
      requireHostSequence(sequence, "file service");
      let event: WireFileEvent;
      try { event = JSON.parse(decoder.decode(data)) as WireFileEvent; } catch { throw new Error("file-service payload is not valid UTF-8 JSON"); }
      if (!event || typeof event !== "object" || typeof event.operationId !== "string") throw new Error("file-service payload is malformed");
      return { kind: "fileService", scope: label, event, sequence };
    }
    case 13: {
      requireHostSequence(sequence, "Git service");
      let event: WireGitEvent;
      try { event = JSON.parse(decoder.decode(data)) as WireGitEvent; } catch { throw new Error("Git-service payload is not valid UTF-8 JSON"); }
      if (!event || typeof event !== "object" || typeof event.rootToken !== "string") throw new Error("Git-service payload is malformed");
      return { kind: "gitService", scope: label, event, sequence };
    }
    case 14: {
      // Every agent frame is a connection-epoch-scoped sideband. Topology and
      // agent frames can share a protocol sequence, so admitting a nonzero
      // header here would create a false gap or duplicate in the terminal hub.
      requireLocalSequence(sequence, "agent service");
      try {
        const payload = JSON.parse(decoder.decode(data)) as WireAgentEvent | WireAgentSnapshot;
        if (label === "snapshot") return { kind: "agentService", scope: label, snapshot: payload as WireAgentSnapshot, sequence };
        const event = payload as WireAgentEvent;
        if (!event || typeof event !== "object" || event.generation === undefined) throw new Error("malformed");
        return { kind: "agentService", scope: label, event, sequence };
      } catch {
        throw new Error("agent-service payload is not valid UTF-8 JSON");
      }
    }
    case 15:
      requireHostSequence(sequence, "terminal flow stall");
      requirePaneId(label, "terminal flow stall");
      try {
        return { kind: "flowStalled", paneId: label, message: decoder.decode(data), sequence };
      } catch {
        throw new Error("terminal flow stall payload is not valid UTF-8");
      }
    default: throw new Error(`unknown terminal frame kind ${frame[0]}`);
  }
}

function decodePaneResource(
  paneId: string,
  sequence: number,
  payload: Uint8Array,
  measurements?: OperationRecorder,
): TerminalEvent {
  requireHostSequence(sequence, "pane resource");
  requirePaneId(paneId, "pane resource");
  if (payload.byteLength < PANE_RESOURCE_HEADER_BYTES) throw new Error("pane resource payload is truncated");
  const state = (["unspecified", "visible", "hiddenBuffered", "released"] as const)[payload[0]];
  if (!state) throw new Error("invalid pane resource state");
  const flags = payload[1];
  if ((flags & ~1) !== 0) throw new Error("pane resource payload has unknown flags");
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const generation = safeBigIntToNumber(view.getBigUint64(2, false), "pane resource generation");
  const snapshotGeneration = safeBigIntToNumber(view.getBigUint64(10, false), "pane resource snapshot generation");
  const tailThroughGeneration = safeBigIntToNumber(view.getBigUint64(18, false), "pane resource tail generation");
  if (snapshotGeneration > tailThroughGeneration || tailThroughGeneration > generation) {
    throw new Error("pane resource generation metadata is inconsistent");
  }
  const reasonLength = view.getUint32(26, false);
  const snapshotLength = view.getUint32(30, false);
  const tailLength = view.getUint32(34, false);
  const expectedLength = PANE_RESOURCE_HEADER_BYTES + reasonLength + snapshotLength + tailLength;
  if (expectedLength !== payload.byteLength) throw new Error("pane resource length fields do not match its payload");
  const reasonEnd = PANE_RESOURCE_HEADER_BYTES + reasonLength;
  const snapshotEnd = reasonEnd + snapshotLength;
  let recoveryReason: string;
  try {
    recoveryReason = decoder.decode(payload.subarray(PANE_RESOURCE_HEADER_BYTES, reasonEnd));
  } catch {
    throw new Error("pane resource recovery reason is not valid UTF-8");
  }
  const serializedSnapshot = copyTerminalBytes(payload.subarray(reasonEnd, snapshotEnd));
  const rawTail = copyTerminalBytes(payload.subarray(snapshotEnd));
  measurements?.add("terminal.decoder.copiedBytes", serializedSnapshot.byteLength + rawTail.byteLength);
  return {
    kind: "paneResource",
    paneId,
    state,
    requiresSeed: Boolean(flags & 1),
    recoveryReason,
    generation,
    snapshotGeneration,
    tailThroughGeneration,
    serializedSnapshot,
    rawTail,
    sequence,
  };
}

function decodeTerminalBytes(
  kind: "seed" | "output",
  paneId: string,
  sequence: number,
  payload: Uint8Array,
  measurements?: OperationRecorder,
): TerminalEvent {
  requireHostSequence(sequence, kind);
  requirePaneId(paneId, kind);
  if (payload.byteLength < 8) throw new Error(`${kind} frame omitted terminal generation`);
  const generation = decodeSafeU64(payload.subarray(0, 8), `${kind} generation`);
  const data = copyTerminalBytes(payload.subarray(8));
  measurements?.add("terminal.decoder.copiedBytes", data.byteLength);
  return { kind, paneId, generation, data, sequence };
}

function decodeSafeU64(bytes: Uint8Array, label: string): number {
  if (bytes.byteLength !== 8) throw new Error(`${label} must be an 8-byte u64`);
  return safeBigIntToNumber(new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getBigUint64(0, false), label);
}

function safeBigIntToNumber(value: bigint, label: string): number {
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`${label} exceeds JavaScript's safe range`);
  return Number(value);
}

function requirePaneId(value: string, context: string): void {
  if (!/^%\d+$/.test(value)) throw new Error(`${context} has an invalid pane label`);
}

function requireLocalSequence(sequence: number, context: string): void {
  if (sequence !== 0) throw new Error(`${context} frame must use local sequence zero`);
}

function requireHostSequence(sequence: number, context: string): void {
  if (sequence === 0) throw new Error(`${context} frame must use a nonzero host sequence`);
}

function requireEmptyPayload(payload: Uint8Array, context: string): void {
  if (payload.byteLength !== 0) throw new Error(`${context} frame has an unexpected payload`);
}

export async function startTerminal(
  sessionId: string,
  paneIds: string[],
  connection: ConnectionSpec,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = (frame) => {
    const admission = startPerfSpan("bridge.jsAdmission");
    recordPerfCounter("bridge.ingressBytes", frame.byteLength);
    recordPerfCounter("desktop.hostEvents");
    try {
      onEvent(decodeTerminalEvent(frame));
    } finally {
      admission();
      if (perfProbeEnabled()) {
        void invoke("acknowledge_bridge_event", { byteLength: frame.byteLength }).catch(() => undefined);
      }
    }
  };
  return measurePerf("workflow.connect", () =>
    invoke<string>("start_terminal", { sessionId, paneIds, connection, onEvent: channel }));
}

export function stopTerminal(clientId: string): Promise<void> {
  return invoke("stop_terminal", { clientId });
}

export function sendInput(clientId: string, paneId: string, data: string): Promise<void> {
  const byteLength = encoder.encode(data).byteLength;
  if (byteLength > MAX_HOST_TERMINAL_INPUT_BYTES) return oversizedTerminalInput(byteLength);
  return measurePerf("invoke.send_terminal_input", () => invoke("send_terminal_input", { clientId, paneId, data }));
}

export function sendBinaryInput(clientId: string, paneId: string, data: Uint8Array): Promise<void> {
  if (data.byteLength > MAX_HOST_TERMINAL_INPUT_BYTES) return oversizedTerminalInput(data.byteLength);
  return measurePerf("invoke.send_terminal_input_bytes", () =>
    invoke("send_terminal_input_bytes", encodeTerminalInputFrame(clientId, paneId, data)));
}

/**
 * Frames binary input as a raw IPC body: `u16` client-id length, client id,
 * `u16` pane-id length, pane id, payload. A `Uint8Array` inside a JSON argument
 * object is serialised as a JSON array of numbers, which is roughly four
 * characters of text per byte, stringified and re-parsed on the main thread.
 */
export function encodeTerminalInputFrame(clientId: string, paneId: string, data: Uint8Array): Uint8Array {
  const client = encoder.encode(clientId);
  const pane = encoder.encode(paneId);
  const frame = new Uint8Array(4 + client.byteLength + pane.byteLength + data.byteLength);
  const view = new DataView(frame.buffer);
  view.setUint16(0, client.byteLength, false);
  frame.set(client, 2);
  const paneOffset = 2 + client.byteLength;
  view.setUint16(paneOffset, pane.byteLength, false);
  frame.set(pane, paneOffset + 2);
  frame.set(data, paneOffset + 2 + pane.byteLength);
  return frame;
}

function oversizedTerminalInput(byteLength: number): Promise<never> {
  return Promise.reject(new Error(
    `Terminal input is ${byteLength} bytes; the 1 MiB atomic input limit prevents sending a partial commit.`,
  ));
}

export function resizeClient(clientId: string, columns: number, rows: number): Promise<void> {
  return invoke("resize_terminal_client", { clientId, columns, rows });
}

/**
 * Tells the host which workspace is on screen, so tmux sizes from that one's
 * control client. `useVisibleTerminalSession.ts` is the only caller and owns
 * why this exists and when it is sent.
 */
export function selectTerminalSession(clientId: string, sessionId: string): Promise<void> {
  return measurePerf("invoke.select_terminal_session", () =>
    invoke("select_terminal_session", { clientId, sessionId }));
}

export function setTerminalVisibility(
  clientId: string,
  paneId: string,
  visible: boolean,
  serializedSnapshot: Uint8Array,
  checkpoint: TerminalVisibilityCheckpoint,
): Promise<void> {
  return measurePerf(visible ? "invoke.set_terminal_visibility.reveal" : "invoke.set_terminal_visibility.hide", () =>
    invoke("set_terminal_visibility",
      encodeTerminalVisibilityFrame(clientId, paneId, visible, serializedSnapshot, checkpoint)));
}

/**
 * Frames a visibility change as a raw IPC body: the input frame's header, then
 * a visibility byte, the terminal epoch and the output cutoff as big-endian
 * `u64`s, then the snapshot bytes.
 *
 * A hide carries the renderer's serialized screen, up to 4 MiB. As a JSON
 * argument that becomes an array of numbers — around 15 MB of text to
 * stringify here and re-parse on the other side, on the thread that is
 * supposed to be painting the tab the user just switched to.
 */
export function encodeTerminalVisibilityFrame(
  clientId: string,
  paneId: string,
  visible: boolean,
  serializedSnapshot: Uint8Array,
  checkpoint: TerminalVisibilityCheckpoint,
): Uint8Array {
  const client = encoder.encode(clientId);
  const pane = encoder.encode(paneId);
  const scalarsOffset = 4 + client.byteLength + pane.byteLength;
  const frame = new Uint8Array(scalarsOffset + 17 + serializedSnapshot.byteLength);
  const view = new DataView(frame.buffer);
  view.setUint16(0, client.byteLength, false);
  frame.set(client, 2);
  const paneOffset = 2 + client.byteLength;
  view.setUint16(paneOffset, pane.byteLength, false);
  frame.set(pane, paneOffset + 2);
  frame[scalarsOffset] = visible ? 1 : 0;
  view.setBigUint64(scalarsOffset + 1, BigInt(checkpoint.terminalEpoch), false);
  view.setBigUint64(scalarsOffset + 9, BigInt(checkpoint.outputGeneration), false);
  frame.set(serializedSnapshot, scalarsOffset + 17);
  return frame;
}

export function requestTerminalSeed(clientId: string, paneId: string): Promise<void> {
  return measurePerf("invoke.request_terminal_seed", () => invoke("request_terminal_seed", { clientId, paneId }));
}

export function terminalBridgeKey(connection: ConnectionSpec, epoch: number): string {
  return `${JSON.stringify(connection)}:${epoch}`;
}

export function terminalBridgeScope(): { sessionId: ""; paneIds: [] } {
  return { sessionId: "", paneIds: [] };
}
