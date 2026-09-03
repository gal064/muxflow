import { Channel, invoke } from "@tauri-apps/api/core";
import { measurePerfRequest, recordPerfCounter } from "../../perf/probe";
import { perfProbeReady } from "../../perf/bootstrap";
import { recordIncident } from "../../diagnostics/incidents";
import type { ConnectionSpec, TmuxSnapshot } from "../../app/types";
import type { WireFileEvent } from "../files/api";
import type { WireGitEvent } from "../git/api";
import type { WireAgentEvent, WireAgentSnapshot } from "../agents/api";
import type { OperationRecorder } from "../../perf/operations";
import { copyTerminalBytes, type OwnedTerminalBytes } from "./TerminalBytes";
import type { RustInputLatencyHistogram } from "./inputLatencyStats";

interface SequencedTerminalEvent {
  sequence: number;
}

export type TerminalEvent = SequencedTerminalEvent & (
  | { kind: "generationEpoch"; epoch: number }
  | { kind: "seed"; paneId: string; generation: number; data: OwnedTerminalBytes }
  | { kind: "output"; paneId: string; generation: number; data: OwnedTerminalBytes }
  | { kind: "seedDiagnostic"; paneId: string; message: string }
  /**
   * One page of the scrollback above a pane's screen, answering one
   * `requestTerminalHistory`. Deliberately not a seed: it carries no
   * generation, because it claims no place in the output ordering — the
   * renderer splices it above what it is already showing, or discards it.
   *
   * `historySize` is how many lines tmux holds for the pane, which is what the
   * renderer compares against the rows it asked for to know whether this page
   * reached the top. It is absent when the host's probe went unanswered — the
   * pane went away mid-request — and that is not the same as a history of zero
   * rows: an absent size means ask again.
   */
  | { kind: "terminalHistory"; paneId: string; data: OwnedTerminalBytes; historySize?: number }
  | { kind: "flowStalled"; paneId: string; message: string }
  | { kind: "flowPaused"; paneId: string; message: string }
  | { kind: "clipboardWrite"; text: string }
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
      /**
       * The host verified its record of this pane's handoff against the
       * reveal's checkpoint: `rawTail` is the complete output since it, and the
       * screen it continues is the one this renderer is already holding. An
       * empty tail is the ordinary answer for a pane that printed nothing while
       * hidden, and it still means "you may draw" — which is why every decision
       * about this answer reads the flag and never the byte count.
       */
      resumeFromRenderer: boolean;
      recoveryReason: string;
      generation: number;
      snapshotGeneration: number;
      tailThroughGeneration: number;
      rawTail: OwnedTerminalBytes;
    }
  /**
   * A topology answer. `snapshot` is absent on a reconciliation
   * acknowledgement — a notified host pass that found the world unchanged and
   * sent the generation alone rather than a tree this process already holds.
   */
  | { kind: "snapshot"; snapshot?: TmuxSnapshot; generation: number; serverIdentity: string; authoritative: boolean }
  | { kind: "fileService"; scope: string; event: WireFileEvent }
  | { kind: "gitService"; scope: string; event: WireGitEvent }
  | { kind: "agentService"; scope: string; event?: WireAgentEvent; snapshot?: WireAgentSnapshot }
);

const decoder = new TextDecoder("utf-8", { fatal: true });
const encoder = new TextEncoder();
export const MAX_HOST_TERMINAL_INPUT_BYTES = 1024 * 1024;
const COMMON_HEADER_BYTES = 11;
const PANE_RESOURCE_HEADER_BYTES = 34;
/** One presence byte and the big-endian `history_size` that follows it. */
const TERMINAL_HISTORY_HEADER_BYTES = 5;

export interface TerminalVisibilityCheckpoint {
  terminalEpoch: number;
  outputGeneration: number;
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
      if (typeof parsed.serverIdentity !== "string" || typeof parsed.authoritative !== "boolean") {
        throw new Error("invalid topology snapshot metadata");
      }
      // No tree at all is the reconciliation acknowledgement: a notified host
      // pass found the world exactly as this process already holds it and sent
      // the generation alone. It closes the reconciliation state and nothing
      // else — see the reducer's snapshot arm.
      if (parsed.snapshot !== undefined && parsed.snapshot !== null && typeof parsed.snapshot !== "object") {
        throw new Error("invalid topology snapshot metadata");
      }
      if (!parsed.authoritative) requireHostSequence(sequence, "ordered topology snapshot");
      const { sequence: _embeddedSequence, snapshot, ...metadata } = parsed;
      return { kind: "snapshot", sequence, snapshot: snapshot ?? undefined, ...metadata };
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
      // An agent snapshot rides along with the topology frame built from the
      // same host event, which already carried the sequence they share, so it
      // arrives local; readmitting that sequence would read as a duplicate. An
      // agent event is a host event of its own that spent its own sequence, and
      // treating it as local would make the next ordered frame look like a gap.
      const snapshotScoped = label === "snapshot";
      if (snapshotScoped) requireLocalSequence(sequence, "agent snapshot");
      else requireHostSequence(sequence, "agent event");
      try {
        const payload = JSON.parse(decoder.decode(data)) as WireAgentEvent | WireAgentSnapshot;
        if (snapshotScoped) return { kind: "agentService", scope: label, snapshot: payload as WireAgentSnapshot, sequence };
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
    case 16:
      requireHostSequence(sequence, "terminal flow pause");
      requirePaneId(label, "terminal flow pause");
      try {
        return { kind: "flowPaused", paneId: label, message: decoder.decode(data), sequence };
      } catch {
        throw new Error("terminal flow pause payload is not valid UTF-8");
      }
    case 17:
      requireHostSequence(sequence, "terminal clipboard write");
      if (label !== "terminal-clipboard" || data.byteLength === 0 || data.byteLength > MAX_HOST_TERMINAL_INPUT_BYTES) {
        throw new Error("terminal clipboard write frame is malformed or oversized");
      }
      try {
        return { kind: "clipboardWrite", text: decoder.decode(data), sequence };
      } catch {
        throw new Error("terminal clipboard write payload is not valid UTF-8");
      }
    case 18: {
      requireHostSequence(sequence, "terminal history");
      requirePaneId(label, "terminal history");
      if (data.byteLength < TERMINAL_HISTORY_HEADER_BYTES) throw new Error("terminal history frame is truncated");
      const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
      const known = view.getUint8(0) === 1;
      const historySize = view.getUint32(1);
      return {
        kind: "terminalHistory",
        paneId: label,
        data: copyTerminalBytes(data.subarray(TERMINAL_HISTORY_HEADER_BYTES)),
        ...(known ? { historySize } : {}),
        sequence,
      };
    }
    default: throw new Error(`unknown terminal frame kind ${frame[0]}`);
  }
}

class BridgeAcknowledgements {
  readonly measurementId = crypto.randomUUID();
  readonly #enabled: boolean;
  #cumulativeFrames = 0;
  #cumulativeBytes = 0;
  #submittedFrames = 0;
  #submittedBytes = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #inFlight: Promise<void> | undefined;
  #closed = false;
  #finalized = false;
  #closePromise: Promise<void> | undefined;
  #finalTotalsWaiter: (() => void) | undefined;

  constructor(enabled: boolean) {
    this.#enabled = enabled;
  }

  record(byteLength: number): void {
    if (this.#closed || !this.#enabled) return;
    this.#cumulativeFrames += 1;
    this.#cumulativeBytes += byteLength;
    this.#finalTotalsWaiter?.();
    this.#schedule(16);
  }

  close(waitForQuiescence = false): Promise<void> {
    this.#closePromise ??= this.#closeOnce(waitForQuiescence);
    return this.#closePromise;
  }

  async #closeOnce(waitForQuiescence: boolean): Promise<void> {
    if (this.#finalized) return;
    if (!this.#enabled) {
      this.#closed = true;
      return;
    }
    const deadline = Date.now() + FINAL_BRIDGE_SHUTDOWN_WAIT_MS;
    const finalTotals = waitForQuiescence ? await this.#waitForQuiescence(deadline) : undefined;
    if (waitForQuiescence && !finalTotals) recordPerfCounter("bridge.finalQuiesceTimeouts");
    if (finalTotals && !(await this.#waitForFinalTotals(finalTotals, deadline))) {
      recordPerfCounter("bridge.finalDeliveryTimeouts");
      recordPerfCounter("bridge.finalDeliveryOutstandingFrames", Math.max(0, finalTotals.cumulativeFrameCount - this.#cumulativeFrames));
      recordPerfCounter("bridge.finalDeliveryOutstandingBytes", Math.max(0, finalTotals.cumulativeByteLength - this.#cumulativeBytes));
    }
    this.#closed = true;
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    this.#timer = undefined;
    if (this.#inFlight) await this.#inFlight.catch(() => undefined);
    for (let attempt = 0; attempt < 3; attempt += 1) {
      if (this.#submittedFrames === this.#cumulativeFrames && this.#submittedBytes === this.#cumulativeBytes) break;
      await this.#send(deadline).catch(() => undefined);
      if (Date.now() >= deadline) break;
    }
    if (!waitForQuiescence || finalTotals) {
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const boundary = { measurementId: this.measurementId };
        try {
          await measurePerfRequest(
            "bridge.finalize", "terminal", boundary,
            (request) => this.#invokeWithinDeadline(invoke("finalize_bridge_measurement", request), deadline),
          );
          this.#finalized = true;
          break;
        } catch {
          // A retained native tombstone keeps this failure observable. Retry the
          // explicit finalization without making terminal shutdown fatal.
        }
        if (Date.now() >= deadline) break;
      }
    }
    if (!this.#finalized) {
      recordPerfCounter("bridge.finalizationIncomplete");
      if (Date.now() >= deadline) recordPerfCounter("bridge.shutdownDeadlineTimeouts");
    }
  }

  async #waitForQuiescence(deadline: number): Promise<BridgeFinalTotals | undefined> {
    while (Date.now() < deadline) {
      const boundary = { measurementId: this.measurementId };
      let totals: BridgeFinalTotals | undefined;
      try {
        await measurePerfRequest(
          "bridge.finalTotals", "terminal", boundary,
          async (request) => {
            const value = await this.#invokeWithinDeadline(
              invoke<BridgeFinalTotals>("bridge_final_totals", request), deadline,
            );
          if (!validBridgeFinalTotals(value)) throw new Error("Native bridge totals were invalid.");
          totals = value;
          },
        );
      } catch {
        // The producer may still be releasing a measured channel. Continue to
        // the one absolute deadline so a missing quiescence signal is explicit.
      }
      if (totals?.quiesced) return totals;
      await new Promise((resolve) => setTimeout(resolve, Math.min(16, Math.max(0, deadline - Date.now()))));
    }
    return undefined;
  }

  async #waitForFinalTotals(expected: BridgeFinalTotals, deadline: number): Promise<boolean> {
    const complete = () => this.#cumulativeFrames >= expected.cumulativeFrameCount
      && this.#cumulativeBytes >= expected.cumulativeByteLength;
    if (complete()) return true;
    return new Promise<boolean>((resolve) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const finish = (delivered: boolean) => {
        if (this.#finalTotalsWaiter === check) this.#finalTotalsWaiter = undefined;
        if (timer !== undefined) clearTimeout(timer);
        resolve(delivered);
      };
      const check = () => { if (complete()) finish(true); };
      this.#finalTotalsWaiter = check;
      timer = setTimeout(
        () => finish(false),
        Math.min(FINAL_BRIDGE_DELIVERY_WAIT_MS, Math.max(0, deadline - Date.now())),
      );
      check();
    });
  }

  async #invokeWithinDeadline<T>(promise: Promise<T>, deadline: number): Promise<T> {
    const remaining = Math.min(MEASUREMENT_INVOKE_WAIT_MS, Math.max(0, deadline - Date.now()));
    if (remaining === 0) {
      recordPerfCounter("bridge.measurementInvokeTimeouts");
      throw new Error("measurement invoke exceeded the shutdown deadline");
    }
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        recordPerfCounter("bridge.measurementInvokeTimeouts");
        reject(new Error("measurement invoke timed out"));
      }, remaining);
      void promise.then(
        (value) => { clearTimeout(timer); resolve(value); },
        (error) => { clearTimeout(timer); reject(error); },
      );
    });
  }

  #schedule(delay: number): void {
    if (this.#timer !== undefined || this.#inFlight || this.#closed) return;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      void this.#send().catch(() => undefined);
    }, delay);
  }

  #send(deadline = Date.now() + MEASUREMENT_INVOKE_WAIT_MS): Promise<void> {
    if (this.#inFlight) return this.#inFlight;
    const cumulativeFrameCount = this.#cumulativeFrames;
    const cumulativeByteLength = this.#cumulativeBytes;
    const boundary = {
      measurementId: this.measurementId,
      cumulativeFrameCount,
      cumulativeByteLength,
    };
    this.#inFlight = measurePerfRequest(
      "bridge.acknowledgement", "terminal", boundary,
      (request) => this.#invokeWithinDeadline(invoke("acknowledge_bridge_events", request), deadline),
    ).then(() => {
      this.#submittedFrames = cumulativeFrameCount;
      this.#submittedBytes = cumulativeByteLength;
    }).finally(() => {
      this.#inFlight = undefined;
      if (this.#submittedFrames !== this.#cumulativeFrames || this.#submittedBytes !== this.#cumulativeBytes) {
        this.#schedule(100);
      }
    });
    return this.#inFlight;
  }
}

/** Production flow credit, distinct from optional performance measurement. */
class DeliveryAcknowledgements {
  #clientId: string | undefined;
  #connectionEpoch: number | undefined;
  #cumulativeFrames = 0;
  #cumulativeBytes = 0;
  #submittedFrames = 0;
  #submittedBytes = 0;
  #inFlight: Promise<void> | undefined;
  #closed = false;

  attach(clientId: string): void {
    this.#clientId = clientId;
    this.#sendIfNeeded();
  }

  beginEpoch(epoch: number): void {
    if (epoch === this.#connectionEpoch) return;
    this.#connectionEpoch = epoch;
    this.#cumulativeFrames = 0;
    this.#cumulativeBytes = 0;
    this.#submittedFrames = 0;
    this.#submittedBytes = 0;
  }

  record(byteLength: number): void {
    if (this.#closed || this.#connectionEpoch === undefined) return;
    this.#cumulativeFrames += 1;
    this.#cumulativeBytes += byteLength;
    // One protocol frame may be the coalesced image of the host's entire
    // 64-record credit window. Make every successfully admitted wire frame an
    // event-driven release opportunity; the single in-flight promise folds a
    // burst into its latest cumulative boundary without a timer.
    this.#sendIfNeeded();
  }

  close(): void {
    this.#closed = true;
  }

  #sendIfNeeded(): void {
    if (this.#closed || this.#inFlight || !this.#clientId || this.#connectionEpoch === undefined) return;
    if (this.#submittedFrames === this.#cumulativeFrames && this.#submittedBytes === this.#cumulativeBytes) return;
    const boundary = {
      clientId: this.#clientId,
      connectionEpoch: this.#connectionEpoch,
      cumulativeFrameCount: this.#cumulativeFrames,
      cumulativeByteLength: this.#cumulativeBytes,
    };
    let delivered = false;
    this.#inFlight = invoke<void>("acknowledge_terminal_delivery", boundary).then(() => {
      delivered = true;
      if (boundary.connectionEpoch !== this.#connectionEpoch) return;
      this.#submittedFrames = boundary.cumulativeFrameCount;
      this.#submittedBytes = boundary.cumulativeByteLength;
    }).finally(() => {
      this.#inFlight = undefined;
      // A failed invoke forces the native bridge to reconnect. If that
      // reconnect's generation event arrived while the old invoke was still
      // settling, it is itself the event-driven wakeup for the fresh epoch.
      // Never retry the failed boundary on the same epoch in a tight loop.
      if (delivered || boundary.connectionEpoch !== this.#connectionEpoch) this.#sendIfNeeded();
    });
    void this.#inFlight.catch(() => undefined);
  }
}

/** Shutdown remains bounded when WebView delivery loses an admitted callback. */
export const FINAL_BRIDGE_DELIVERY_WAIT_MS = 1_000;
export const FINAL_BRIDGE_SHUTDOWN_WAIT_MS = 2_000;
const MEASUREMENT_INVOKE_WAIT_MS = 250;

const bridgeAcknowledgements = new Map<string, BridgeAcknowledgements>();
const deliveryAcknowledgements = new Map<string, DeliveryAcknowledgements>();
/**
 * How each live client was connected, for the concurrency journal only.
 *
 * Two live clients are legitimate while a host switch is in flight — the old
 * bridge is stopped asynchronously while the new one is already starting — so
 * the count alone cannot separate that from a leak. The mode is the cheapest
 * thing at the call site that tells "the same host, twice" apart from "a local
 * host and a remote one", and unlike the profile id or the target it carries no
 * hostname into the journal.
 */
const clientConnectionModes = new Map<string, ConnectionSpec["mode"]>();

interface BridgeFinalTotals {
  cumulativeFrameCount: number;
  cumulativeByteLength: number;
  quiesced: boolean;
}

function validBridgeFinalTotals(value: BridgeFinalTotals | undefined): value is BridgeFinalTotals {
  return Boolean(value
    && Number.isSafeInteger(value.cumulativeFrameCount)
    && value.cumulativeFrameCount >= 0
    && Number.isSafeInteger(value.cumulativeByteLength)
    && value.cumulativeByteLength >= 0
    && typeof value.quiesced === "boolean");
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
  if ((flags & ~3) !== 0) throw new Error("pane resource payload has unknown flags");
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const generation = safeBigIntToNumber(view.getBigUint64(2, false), "pane resource generation");
  const snapshotGeneration = safeBigIntToNumber(view.getBigUint64(10, false), "pane resource snapshot generation");
  const tailThroughGeneration = safeBigIntToNumber(view.getBigUint64(18, false), "pane resource tail generation");
  if (snapshotGeneration > tailThroughGeneration || tailThroughGeneration > generation) {
    throw new Error("pane resource generation metadata is inconsistent");
  }
  const reasonLength = view.getUint32(26, false);
  const tailLength = view.getUint32(30, false);
  const expectedLength = PANE_RESOURCE_HEADER_BYTES + reasonLength + tailLength;
  if (expectedLength !== payload.byteLength) throw new Error("pane resource length fields do not match its payload");
  const reasonEnd = PANE_RESOURCE_HEADER_BYTES + reasonLength;
  let recoveryReason: string;
  try {
    recoveryReason = decoder.decode(payload.subarray(PANE_RESOURCE_HEADER_BYTES, reasonEnd));
  } catch {
    throw new Error("pane resource recovery reason is not valid UTF-8");
  }
  const rawTail = copyTerminalBytes(payload.subarray(reasonEnd));
  measurements?.add("terminal.decoder.copiedBytes", rawTail.byteLength);
  return {
    kind: "paneResource",
    paneId,
    state,
    requiresSeed: Boolean(flags & 1),
    resumeFromRenderer: Boolean(flags & 2),
    recoveryReason,
    generation,
    snapshotGeneration,
    tailThroughGeneration,
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

/**
 * Turns one wire frame into one delivered event, and never escapes with the
 * frame unaccounted for.
 *
 * The channel dispatcher in `@tauri-apps/api` advances its own message index
 * around this call: a throw that reaches it stops that index from advancing, so
 * every later frame queues behind the failure forever and the pane is frozen
 * until the app restarts. A malformed or invalid frame must therefore be
 * *dropped* here, not raised: the frame already passed the native contiguity
 * check before it was encoded, so the hole this leaves is seen by the hub's
 * own watermark, whose recovery path — however blunt — recovers, while a
 * wedged channel does not.
 *
 * Credit is charged per wire frame regardless of whether the frame could be
 * decoded, so the acknowledgements are recorded on the failure path too;
 * skipping them would leak the host's window one undecodable frame at a time.
 */
function handleTerminalFrame(
  frame: ArrayBuffer,
  onEvent: (event: TerminalEvent) => void,
  acknowledgements: BridgeAcknowledgements,
  delivery: DeliveryAcknowledgements,
): void {
  recordPerfCounter("bridge.ingressBytes", frame.byteLength);
  recordPerfCounter("desktop.hostEvents");
  let event: TerminalEvent;
  try {
    event = decodeTerminalEvent(frame);
  } catch (error) {
    acknowledgements.record(frame.byteLength);
    delivery.record(frame.byteLength);
    recordIncident("link.decodeFailure", { message: String(error) });
    return;
  }
  if (event.kind === "generationEpoch") delivery.beginEpoch(event.epoch);
  // Decoding is the ownership boundary: once a complete wire frame becomes
  // a typed event, native and host credit must eventually be released even
  // if an application observer rejects the event. A callback failure may be
  // surfaced (and the app's hub turns observer failures into reconnects),
  // but it cannot punch an ordinal hole that a later cumulative ACK crosses.
  try {
    onEvent(event);
  } finally {
    acknowledgements.record(frame.byteLength);
    delivery.record(frame.byteLength);
  }
}

export async function startTerminal(
  sessionId: string,
  paneIds: string[],
  connection: ConnectionSpec,
  /**
   * Whether the bridge attaches a terminal at all. A host shown beside the
   * active one relays topology and agents only, until `selectTerminalSession`
   * names a session on it.
   */
  attach: boolean,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const measurementEnabled = await perfProbeReady();
  const channel = new Channel<ArrayBuffer>();
  const acknowledgements = new BridgeAcknowledgements(measurementEnabled);
  const delivery = new DeliveryAcknowledgements();
  channel.onmessage = (frame) => handleTerminalFrame(frame, onEvent, acknowledgements, delivery);
  // Held outside the try so the failure path can still reach a client the
  // native side has already started: see `abandonStartedClient`.
  let startedClientId: string | undefined;
  try {
    const startRequest = {
      sessionId, paneIds, connection, attach, measurementId: acknowledgements.measurementId,
    };
    const boundary = { ...startRequest, onEvent: channel };
    const clientId = await measurePerfRequest("workflow.connect", "terminal", boundary, async (request) => {
      const value = await invoke<string>("start_terminal", request);
      if (!value) throw new Error("Native terminal startup omitted its client ID.");
      // Recorded here rather than from the awaited result: the client is alive
      // the instant this resolves, and the request boundary keeps accounting
      // afterwards, so a throw from the boundary itself must still find the id.
      startedClientId = value;
      return value;
    });
    bridgeAcknowledgements.set(clientId, acknowledgements);
    delivery.attach(clientId);
    deliveryAcknowledgements.set(clientId, delivery);
    clientConnectionModes.set(clientId, connection.mode);
    journalConcurrentClients();
    return clientId;
  } catch (error) {
    if (startedClientId !== undefined) abandonStartedClient(startedClientId);
    delivery.close();
    await acknowledgements.close();
    throw error;
  }
}

/**
 * Stops a native client whose id the caller will never learn.
 *
 * The invariant: no native client may outlive the caller's knowledge of its id.
 * `start_terminal` resolving is the point the native side owns a live supervisor
 * thread — it holds the ssh carrier, reconnects on its own backoff, mints
 * connection epochs and attaches tmux control clients — and `stop_terminal` is
 * keyed by that id alone. Anything that throws between the id being minted and
 * `startTerminal` returning it therefore has exactly one place left where the
 * client can still be reached: here.
 *
 * The registrations are dropped first so the caller's own close of the channels
 * stays the only one, and the stop itself is fire-and-forget — the caller must
 * see the startup failure, not a secondary failure to clean up after it, so a
 * failing stop is journalled rather than raised.
 */
function abandonStartedClient(clientId: string): void {
  bridgeAcknowledgements.delete(clientId);
  deliveryAcknowledgements.delete(clientId);
  clientConnectionModes.delete(clientId);
  const boundary = { clientId };
  void measurePerfRequest(
    "terminal.stop", "terminal", boundary, (request) => invoke<void>("stop_terminal", request),
  ).catch((error) => recordIncident("terminal.abandonedClientStopFailed", { message: String(error) }));
}

/**
 * Journals the moment a second native client becomes live.
 *
 * Every extra client is an independent ssh carrier, reconnect schedule and tmux
 * control attachment, so "how many bridges were actually running" is the first
 * question any reconnect-storm report has to answer — and nothing in the app
 * could answer it before. One line per registration that finds company is
 * enough: the ids are opaque native UUIDs, and the modes say whether the
 * overlap is the legitimate kind (a local host beside a remote one, or a host
 * switch whose old bridge has not finished stopping) or the same host twice.
 */
function journalConcurrentClients(): void {
  const clientIds = [...bridgeAcknowledgements.keys()];
  if (clientIds.length <= 1) return;
  recordIncident("terminal.multiClient", {
    count: clientIds.length,
    clientIds,
    modes: clientIds.map((id) => clientConnectionModes.get(id) ?? "unknown"),
  });
}

export async function stopTerminal(clientId: string): Promise<void> {
  const acknowledgements = bridgeAcknowledgements.get(clientId);
  const delivery = deliveryAcknowledgements.get(clientId);
  let stopped = false;
  try {
    const boundary = { clientId };
    await measurePerfRequest(
      "terminal.stop", "terminal", boundary, (request) => invoke<void>("stop_terminal", request),
    );
    stopped = true;
  } finally {
    bridgeAcknowledgements.delete(clientId);
    deliveryAcknowledgements.delete(clientId);
    clientConnectionModes.delete(clientId);
    delivery?.close();
    await acknowledgements?.close(stopped);
  }
}

export function sendInput(clientId: string, paneId: string, data: string): Promise<void> {
  const byteLength = encoder.encode(data).byteLength;
  if (byteLength > MAX_HOST_TERMINAL_INPUT_BYTES) return oversizedTerminalInput(byteLength);
  const boundary = { clientId, paneId, data };
  return measurePerfRequest(
    "invoke.send_terminal_input", "terminal", boundary, (request) => invoke("send_terminal_input", request),
  );
}

export function sendBinaryInput(clientId: string, paneId: string, data: Uint8Array): Promise<void> {
  if (data.byteLength > MAX_HOST_TERMINAL_INPUT_BYTES) return oversizedTerminalInput(data.byteLength);
  const frame = encodeTerminalInputFrame(clientId, paneId, data);
  return measurePerfRequest(
    "invoke.send_terminal_input_bytes", "terminal", frame,
    (request) => invoke("send_terminal_input_bytes", request), { encoding: "raw" },
  );
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
  const boundary = { clientId, columns, rows };
  return measurePerfRequest(
    "invoke.resize_terminal_client", "terminal", boundary, (request) => invoke("resize_terminal_client", request),
  );
}

/**
 * Tells the host which workspace is on screen, so tmux sizes from that one's
 * control client. `useVisibleTerminalSession.ts` is the only caller and owns
 * why this exists and when it is sent.
 */
export function selectTerminalSession(clientId: string, sessionId: string): Promise<void> {
  const boundary = { clientId, sessionId };
  return measurePerfRequest(
    "invoke.select_terminal_session", "terminal", boundary, (request) => invoke("select_terminal_session", request),
  );
}

export function setTerminalVisibility(
  clientId: string,
  paneId: string,
  visible: boolean,
  rendererHoldsSnapshot: boolean,
  checkpoint: TerminalVisibilityCheckpoint,
): Promise<void> {
  const frame = encodeTerminalVisibilityFrame(clientId, paneId, visible, rendererHoldsSnapshot, checkpoint);
  return measurePerfRequest(
    visible ? "invoke.set_terminal_visibility.reveal" : "invoke.set_terminal_visibility.hide",
    "terminal",
    frame,
    (request) => invoke("set_terminal_visibility", request),
    { encoding: "raw" },
  );
}

/**
 * Frames a visibility change as a raw IPC body: the input frame's header, then
 * a visibility byte, a flags byte, and the terminal epoch and output cutoff as
 * big-endian `u64`s.
 *
 * A hide used to carry the renderer's serialized screen, up to 4 MiB of it,
 * which is why this body is raw rather than JSON — an array of numbers that
 * size is around 15 MB of text to stringify here and re-parse on the other
 * side, on the thread that is supposed to be painting the tab the user just
 * switched to. It carries no screen now: bit 0 of the flags byte says the
 * renderer kept its own, which is what lets the host answer the reveal with the
 * output since the checkpoint instead of a copy of the screen. The body stays
 * raw because the frame is still on the switch path and a JSON round trip there
 * costs more than the frame does.
 */
export function encodeTerminalVisibilityFrame(
  clientId: string,
  paneId: string,
  visible: boolean,
  rendererHoldsSnapshot: boolean,
  checkpoint: TerminalVisibilityCheckpoint,
): Uint8Array {
  const client = encoder.encode(clientId);
  const pane = encoder.encode(paneId);
  const scalarsOffset = 4 + client.byteLength + pane.byteLength;
  const frame = new Uint8Array(scalarsOffset + 18);
  const view = new DataView(frame.buffer);
  view.setUint16(0, client.byteLength, false);
  frame.set(client, 2);
  const paneOffset = 2 + client.byteLength;
  view.setUint16(paneOffset, pane.byteLength, false);
  frame.set(pane, paneOffset + 2);
  frame[scalarsOffset] = visible ? 1 : 0;
  frame[scalarsOffset + 1] = rendererHoldsSnapshot ? 1 : 0;
  view.setBigUint64(scalarsOffset + 2, BigInt(checkpoint.terminalEpoch), false);
  view.setBigUint64(scalarsOffset + 10, BigInt(checkpoint.outputGeneration), false);
  return frame;
}

/**
 * Asks the host for the scrollback above a pane's screen.
 *
 * Separate from `requestTerminalSeed` because it asks a different question: a
 * seed request also asserts that this pane is visible and settles its seed
 * debt, and a photograph of the scrollback does neither.
 *
 * `skipLines` is the scrollback this renderer is already holding. tmux measures
 * its capture from the pane's current display, so a pane that has printed since
 * it was seeded would be handed the rows that scrolled off in the meantime a
 * second time, and the splice would show them twice.
 */
export function requestTerminalHistory(
  clientId: string,
  paneId: string,
  lines: number,
  skipLines: number,
): Promise<void> {
  const boundary = { clientId, paneId, lines, skipLines };
  return measurePerfRequest(
    "invoke.request_terminal_history", "terminal", boundary, (request) => invoke("request_terminal_history", request),
  );
}

export function requestTerminalSeed(clientId: string, paneId: string): Promise<void> {
  const boundary = { clientId, paneId };
  return measurePerfRequest(
    "invoke.request_terminal_seed", "terminal", boundary, (request) => invoke("request_terminal_seed", request),
  );
}

/** The native half of one echo-lag journal line — see `terminal_link_stats`. */
export interface TerminalLinkStats {
  reservedBytes: number;
  ackedBytes: number;
  reservedRecords: number;
  ackedRecords: number;
  msSinceLastHostEvent: number;
  /** Requests the native side gave up waiting on, since this client started. */
  lateRequestsTotal: number;
  /**
   * Bytes and frames the native reader has taken off the ssh stream since this
   * connection started. Absent unless the process is running a measured build
   * with `ADE_PERF_LOG` set: sampled at a keystroke and again at its echo, the
   * difference is how much other traffic the echo waited behind.
   */
  bytesReadTotal?: number;
  framesReadTotal?: number;
}

/**
 * Reads the delivery link's counters for the incident journal.
 *
 * Deliberately unmeasured and never rejecting: this runs while the app is
 * already struggling, and a diagnostic that can fail the path it is describing
 * is worse than no diagnostic. `null` means "stats unavailable".
 */
export async function fetchLinkStats(clientId: string): Promise<TerminalLinkStats | null> {
  try {
    return await invoke<TerminalLinkStats>("terminal_link_stats", { clientId });
  } catch {
    return null;
  }
}

/**
 * Reads and drains the native input queue's latency histogram.
 *
 * Same contract as `fetchLinkStats`: unmeasured, never rejecting, `null` when
 * the stats are unavailable. The native command drains what it returns, so a
 * caller that discards the answer discards that window's counts.
 */
export async function fetchInputLatencyStats(clientId: string): Promise<RustInputLatencyHistogram | null> {
  try {
    return await invoke<RustInputLatencyHistogram | null>("input_latency_stats", { clientId });
  } catch {
    return null;
  }
}

export function terminalBridgeKey(connection: ConnectionSpec, epoch: number): string {
  return `${JSON.stringify(connection)}:${epoch}`;
}

/**
 * What a new bridge asks the host to attach first.
 *
 * The session is the one the shell is showing, so a rebuild — a resume, a
 * changed server — lands the host on that workspace directly. Left empty, the
 * host attaches its first session instead and the shell has to move it back
 * afterwards: a round trip the user watches as the wrong workspace. The panes
 * stay empty; the host mounts the attached session's active window, and the
 * shell's own visibility requests refine that once the tabs are up.
 */
export function terminalBridgeScope(sessionId?: string): { sessionId: string; paneIds: [] } {
  return { sessionId: sessionId ?? "", paneIds: [] };
}
