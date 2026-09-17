// The connection state machine (design doc §7): framing, handshake, ordered
// events, requests, credit acks, reconnect. Talks to the host through a
// `Transport` so it runs unchanged over the SSH module, an in-memory pipe, or
// a child process.

import { create } from "@bufbuild/protobuf";
import { OutputCreditLedger } from "./credit";
import { PROTOCOL_MAJOR, validateHostContract } from "./contract";
import { FrameAccumulator, encodeFrame } from "./framing";
import {
  ClientHelloSchema,
  EnvelopeSchema,
  EventKind,
  Priority,
  TerminalOutputAckSchema,
  type Envelope,
  type FileStreamFrame,
  type HostEvent,
  type Request,
  type Response,
  type ServerHello,
  type VoiceResponse,
} from "./gen/envelope_pb";
import { logAgentTransitions } from "../features/agents/diagnostics";
import { deriveAgentIdentityPromotion, type AgentIdentityPromotion } from "../features/agents/agentIdentityPromotion";
import { requestTerminalSeed, subscribeFull } from "./requests";
import { jsBackgroundTimer, type BackgroundTimer, type BackgroundTimerHandle } from "./backgroundTimer";
import { TransportDialError, type Transport, type TransportClose, type TransportCloseReason } from "./Transport";
import type { AgentTransition, ConnectionState, SavedHostRef, SessionStore } from "../store/sessionStore";

export const REQUEST_TIMEOUT_MS = 20_000;
/**
 * A late answer fails only its request. The lane is torn down only after this
 * many unanswered requests in a row *and* `STALLED_LANE_SILENCE_MS` without
 * any answer — the desktop's rule (`connection.rs`, docs/bugs/slow-link.md):
 * on a slow link a workspace switch can legitimately answer 8 s late, and
 * dropping the lane for it re-sends every screen and makes the next answer
 * late too.
 */
export const STALLED_LANE_UNANSWERED_REQUESTS = 3;
export const STALLED_LANE_SILENCE_MS = 15_000;
export const FILE_STREAM_TIMEOUT_MS = 60_000;
/** A connection `connected` this long resets the reconnect attempt counter (§7.2). */
export const STABLE_AFTER_MS = 60_000;
export const MAX_BACKOFF_MS = 30_000;
/** Bound on ServerHello + Subscribe response; the host bounds its own side at 5 s (service.rs). */
export const HANDSHAKE_TIMEOUT_MS = 20_000;

/** §7.2: closes after which the app does not retry. */
const FATAL_CLOSE_REASONS: ReadonlySet<TransportCloseReason> = new Set(["authFailed", "hostKeyMismatch", "hostKeyNotTrusted"]);
const HELPER_MISSING_EXIT_CODE = 127;

export class HostError extends Error {
  /**
   * `Response.voice` of a refused voice operation: the echoed `operationId`
   * and `retryable`, which live there because `Response` has no retry flag
   * (docs/mobile/voice-mode-plan.md §3). Undefined for every other refusal.
   */
  constructor(readonly code: string, message: string, readonly voice?: VoiceResponse) {
    super(message);
    this.name = "HostError";
  }
}

export class RequestTimeoutError extends Error {
  constructor(readonly requestId: bigint) {
    super("The host didn't answer in time.");
    this.name = "RequestTimeoutError";
  }
}

export class ConnectionClosedError extends Error {
  constructor(message = "connection closed") {
    super(message);
    this.name = "ConnectionClosedError";
  }
}

/** Where terminal bytes go (§7.4, §7.6). The terminal feature (M3) implements it. */
export interface TerminalSink {
  seed(paneId: string, bytes: Uint8Array, generation: bigint): void;
  output(paneId: string, bytes: Uint8Array, generation: bigint): void;
  /**
   * The host emits TERMINAL_EXIT scoped by **session id** ("$N"), not pane id:
   * its only emitter is `reconcile_terminal_clients_locked` in
   * apps/host/src/service.rs, when a session's control client failed. Every
   * pane of that session is affected.
   */
  exit(sessionId: string, detail: string): void;
  /**
   * One TERMINAL_HISTORY answer (§7.6.1): the rows above the screen, plus
   * tmux's own history size when the targeted probe answered (`sizeKnown`
   * false means "ask again", not "no scrollback").
   */
  history?(paneId: string, bytes: Uint8Array, historySize: number, sizeKnown: boolean): void;
}

export interface RequestOptions {
  timeoutMs?: number;
  /** Receives every `fileStream` frame carried on this request's id (§11.1). */
  onFileStream?: (frame: FileStreamFrame) => void;
}

/**
 * A second connection bound to a control connection (§11.1). The host serves
 * file bodies (OPEN_FILE_STREAM) only on such a lane
 * (apps/host/src/service/requests/operation_policy.rs, `Lane::Bulk`), and
 * serves nothing else on it: no Subscribe, no events, no terminal.
 */
export interface BulkBinding {
  /** The control connection's `serverHello.serverIdentity`. */
  expectedServerIdentity: string;
  /** The control connection's `connectionEpoch`; the host echoes it back. */
  connectionEpoch: bigint;
}

export interface HostConnectionOptions {
  /**
   * Opens the SSH exec channel (or whatever pipe) to `muxflow-host bridge --stdio`.
   * `signal` aborts when the user disconnects mid-dial: an SSH login can be
   * held open indefinitely (Tailscale SSH check mode), and nothing else could
   * reach a channel that has not produced a transport yet.
   */
  dial: (signal: AbortSignal) => Promise<Transport>;
  /** Persisted per saved host, incremented on every attempt; must return >= 1. */
  nextConnectionEpoch: () => number | bigint;
  /**
   * The session store this connection reports into. A bulk connection must be
   * given a private store (`createSessionStore()`): it only ever writes the
   * `connection` slice, and must not clobber the control connection's.
   */
  store: SessionStore;
  /** Present for a bulk connection; absent for the control connection. */
  bulk?: BulkBinding;
  host?: SavedHostRef;
  terminals?: Partial<TerminalSink>;
  /** Every applied AGENT_STATE upsert and every agent in a reconciling snapshot (§13). */
  onAgentTransition?: (transition: AgentTransition) => void;
  /** One atomic pane-derived → native agent identity handoff. */
  onAgentIdentityPromotion?: (promotion: AgentIdentityPromotion) => void;
  /** A live topology snapshot replaced the tmux server behind this connection. */
  onServerIdentityChanged?: (serverIdentity: string) => void;
  /** ACTIVE_ROOT, DIRECTORY_SNAPSHOT, FILE_CHANGED (§11). */
  onFileEvent?: (event: HostEvent) => void;
  /** VOICE_PROVISION, VOICE_REPLY (docs/mobile/voice-mode-plan.md); payload in `event.voice`. */
  onVoiceEvent?: (event: HostEvent) => void;
  onToast?: (message: string) => void;
  /** Fires on every transition to `connected`; open terminals re-attach here (§7.6 step 5). */
  onConnected?: () => void;
  log?: (line: string) => void;
  /**
   * Runs the §7.2 backoff, the handshake deadline and the 60 s stable timer.
   * The app passes the native timer (`backgroundTimer()`): React Native
   * freezes JS timers while the activity is paused, and a reconnect is exactly
   * the delay that has to elapse with the app in the background — as is the
   * deadline on the handshake that reconnect starts, and the stability window
   * that resets the exponent (a stable timer frozen in the background never
   * fires at all, so a phone flapping there climbs to the cap and stays).
   * Defaults to `setTimeout`. The request timers stay on `setTimeout` on
   * purpose: one bridge call per request is not worth it.
   */
  reconnectTimer?: BackgroundTimer;
  requestTimeoutMs?: number;
  fileStreamTimeoutMs?: number;
  stableAfterMs?: number;
  maxBackoffMs?: number;
  handshakeTimeoutMs?: number;
}

interface PendingRequest {
  resolve: (response: Response) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  onFileStream?: ((frame: FileStreamFrame) => void) | undefined;
  /** `file.operationId` of the request, when it carries one. */
  operationId?: string | undefined;
}

type Phase = "hello" | "subscribe" | "live";

/** Per-attempt state, replaced wholesale on every (re)connect. */
interface Attempt {
  token: number;
  transport: Transport;
  accumulator: FrameAccumulator;
  phase: Phase;
  buffered: Envelope[];
  hello?: ServerHello;
  lastSequence: bigint;
  nextRequestId: bigint;
  pending: Map<bigint, PendingRequest>;
  credit?: OutputCreditLedger;
  connectionEpoch: bigint;
  handshakeTimer?: BackgroundTimerHandle | undefined;
  /** Requests that missed their deadline since the host last answered one. */
  unansweredRequests: number;
  /** When the host last answered a request — or when this lane started, so a fresh lane is owed the full silence. */
  lastAnswerAt: number;
  /** Set once we decided the outcome of this attempt, so its close is not re-diagnosed. */
  outcome?: { state: "failed" | "incompatible"; message: string } | { state: "reconnect"; message: string };
}

export class HostConnection {
  private attempt: Attempt | undefined;
  private attemptCounter = 0;
  /** Cancels the dial in flight, if any, when the user disconnects before it produced a transport. */
  private dialAbort: AbortController | undefined;
  private reconnectAttempt = 0;
  private reconnectTimer: BackgroundTimerHandle | undefined;
  private stableTimer: BackgroundTimerHandle | undefined;
  private wantConnected = false;
  private readonly timer: BackgroundTimer;
  private readonly requestTimeoutMs: number;
  private readonly fileStreamTimeoutMs: number;
  private readonly stableAfterMs: number;
  private readonly maxBackoffMs: number;
  private readonly handshakeTimeoutMs: number;

  constructor(private readonly options: HostConnectionOptions) {
    this.timer = options.reconnectTimer ?? jsBackgroundTimer;
    this.requestTimeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
    this.fileStreamTimeoutMs = options.fileStreamTimeoutMs ?? FILE_STREAM_TIMEOUT_MS;
    this.stableAfterMs = options.stableAfterMs ?? STABLE_AFTER_MS;
    this.maxBackoffMs = options.maxBackoffMs ?? MAX_BACKOFF_MS;
    this.handshakeTimeoutMs = options.handshakeTimeoutMs ?? HANDSHAKE_TIMEOUT_MS;
  }

  get state(): ConnectionState {
    return this.options.store.getState().connection.state;
  }

  get serverHello(): ServerHello | undefined {
    return this.attempt?.hello;
  }

  /**
   * The tmux server identity every guarded request must echo (§7.5). The
   * latest TOPOLOGY_SNAPSHOT wins over the ServerHello: a tmux server that
   * starts after the handshake changes the identity from `tmux:none`, and
   * the host refuses the stale one with `stale_topology`.
   */
  get serverIdentity(): string {
    const fromTopology = this.options.store.getState().serverIdentity;
    return fromTopology || (this.attempt?.hello?.serverIdentity ?? "");
  }

  /** The epoch sent in this connection's ClientHello; also the visibility checkpoint epoch (§7.6). */
  get connectionEpoch(): bigint {
    return this.attempt?.connectionEpoch ?? 0n;
  }

  connect(): void {
    if (this.wantConnected && this.attempt) return;
    this.wantConnected = true;
    this.clearReconnectTimer();
    this.reconnectAttempt = 0;
    this.options.store.getState().setConnection({ state: "sshConnecting", attempt: 0, message: undefined, ...(this.options.host ? { host: this.options.host } : {}) });
    void this.open();
  }

  /** User-initiated: no reconnect follows. */
  disconnect(): void {
    this.wantConnected = false;
    this.clearReconnectTimer();
    this.clearStableTimer();
    this.dialAbort?.abort();
    const attempt = this.attempt;
    if (attempt) {
      this.teardown(attempt, new ConnectionClosedError("disconnected"));
      attempt.transport.close();
    }
    this.options.store.getState().setConnection({ state: "idle", attempt: 0, message: undefined });
  }

  /** Sends one request and settles on its response (§7.5). */
  request(request: Request, options: RequestOptions = {}): Promise<Response> {
    const attempt = this.attempt;
    if (!attempt || attempt.phase !== "live") {
      return Promise.reject(new ConnectionClosedError("not connected"));
    }
    const requestId = attempt.nextRequestId++;
    const timeoutMs = options.timeoutMs ?? (options.onFileStream ? this.fileStreamTimeoutMs : this.requestTimeoutMs);
    return new Promise<Response>((resolve, reject) => {
      const timer = setTimeout(() => {
        attempt.pending.delete(requestId);
        reject(new RequestTimeoutError(requestId));
        // A missed deadline is never replayed (the host may still complete
        // it), but on its own it only fails this request. The lane is dropped
        // once the host has stopped answering altogether: three misses in a
        // row and nothing answered for 15 s. The supervisor then reconnects
        // and reconciles from a fresh snapshot.
        if (this.options.bulk) return;
        attempt.unansweredRequests += 1;
        const silence = Date.now() - attempt.lastAnswerAt;
        if (attempt.unansweredRequests >= STALLED_LANE_UNANSWERED_REQUESTS && silence >= STALLED_LANE_SILENCE_MS) {
          this.reconnectNow(attempt, "host stopped answering; reconnecting");
        } else {
          this.log(`request.late requestId=${requestId} unanswered=${attempt.unansweredRequests} silenceMs=${silence}`);
        }
      }, timeoutMs);
      attempt.pending.set(requestId, {
        resolve,
        reject,
        timer,
        onFileStream: options.onFileStream,
        operationId: request.file?.operationId || undefined,
      });
      try {
        this.send(attempt, envelope(requestId, { case: "request", value: request }));
      } catch (error) {
        clearTimeout(timer);
        attempt.pending.delete(requestId);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  // ---- connection lifecycle -------------------------------------------------

  private async open(): Promise<void> {
    const token = ++this.attemptCounter;
    const dialAbort = new AbortController();
    this.dialAbort = dialAbort;
    let transport: Transport;
    try {
      transport = await this.options.dial(dialAbort.signal);
    } catch (error) {
      if (this.dialAbort === dialAbort) this.dialAbort = undefined;
      if (token !== this.attemptCounter || !this.wantConnected) return;
      const close: TransportClose = error instanceof TransportDialError
        ? error.close
        : { reason: "connectFailed", message: error instanceof Error ? error.message : String(error) };
      this.afterClose(close);
      return;
    }
    if (this.dialAbort === dialAbort) this.dialAbort = undefined;
    if (token !== this.attemptCounter || !this.wantConnected) {
      transport.close();
      return;
    }
    const attempt: Attempt = {
      token,
      transport,
      accumulator: new FrameAccumulator(),
      phase: "hello",
      buffered: [],
      lastSequence: 0n,
      nextRequestId: 3n,
      pending: new Map(),
      connectionEpoch: this.options.bulk?.connectionEpoch ?? BigInt(this.options.nextConnectionEpoch()),
      unansweredRequests: 0,
      lastAnswerAt: Date.now(),
    };
    this.attempt = attempt;
    transport.onData((chunk) => {
      if (this.attempt === attempt) this.onData(attempt, chunk);
    });
    transport.onClosed((close) => {
      if (this.attempt === attempt) this.onClosed(attempt, close);
    });
    this.options.store.getState().setConnection({ state: "handshaking" });
    // On the background timer too: a reconnect fired from the background
    // dials natively, and a bridge that never answers would otherwise hold
    // `handshaking` until the app is next opened.
    attempt.handshakeTimer = this.timer.set(this.handshakeTimeoutMs, () => {
      attempt.handshakeTimer = undefined;
      if (this.attempt === attempt && attempt.phase !== "live") {
        this.log(`handshake.timeout phase=${attempt.phase}`);
        this.reconnectNow(attempt, "handshake timed out");
      }
    });
    try {
      this.sendHandshake(attempt);
    } catch (error) {
      // The transport died between dial and the first write; its own closed
      // event may or may not follow, so diagnose it here.
      this.log(`handshake.write.failed ${error instanceof Error ? error.message : String(error)}`);
      if (this.attempt === attempt) {
        this.teardown(attempt, new ConnectionClosedError("transport write failed"));
        transport.close();
        if (!attempt.outcome) this.afterClose({ reason: "exited", message: error instanceof Error ? error.message : String(error) });
      }
    }
  }

  private sendHandshake(attempt: Attempt): void {
    if (attempt.connectionEpoch < 1n) throw new Error("connectionEpoch must be >= 1");
    // §7.3: both handshake frames go out without waiting between them.
    const bulk = this.options.bulk;
    const hello = create(ClientHelloSchema, {
      bulkConnection: bulk !== undefined,
      expectedServerIdentity: bulk?.expectedServerIdentity ?? "",
      connectionEpoch: attempt.connectionEpoch,
    });
    this.send(attempt, envelope(1n, { case: "clientHello", value: hello }));
    // A bulk lane never subscribes: Subscribe is a control operation the host
    // refuses there, and no events are registered for it.
    if (!bulk) this.send(attempt, envelope(2n, { case: "request", value: subscribeFull() }));
  }

  private send(attempt: Attempt, frame: Envelope): void {
    attempt.transport.write(encodeFrame(frame));
  }

  private onData(attempt: Attempt, chunk: Uint8Array): void {
    try {
      attempt.accumulator.push(chunk);
      for (;;) {
        const frame = attempt.accumulator.nextFrame();
        if (!frame) break;
        this.onFrame(attempt, frame);
        if (this.attempt !== attempt) break;
      }
    } catch (error) {
      this.log(`protocol.error ${error instanceof Error ? error.message : String(error)}`);
      this.reconnectNow(attempt, "protocol error");
    }
  }

  private onFrame(attempt: Attempt, frame: Envelope): void {
    switch (attempt.phase) {
      case "hello":
        this.onHelloFrame(attempt, frame);
        return;
      case "subscribe":
        if (frame.payload.case === "response" && frame.requestId === 2n) {
          this.onSubscribeResponse(attempt, frame.payload.value);
        } else {
          attempt.buffered.push(frame);
        }
        return;
      case "live":
        this.onLiveFrame(attempt, frame);
    }
  }

  private onHelloFrame(attempt: Attempt, frame: Envelope): void {
    const refusal = validateHostContract(frame.protocolMajor);
    if (refusal) {
      this.log(`handshake.incompatible ${refusal.kind}`);
      this.fail(attempt, "incompatible", refusal.message);
      return;
    }
    if (frame.payload.case === "error") {
      this.fail(attempt, "failed", frame.payload.value.displayMessage);
      return;
    }
    if (frame.payload.case !== "serverHello") {
      this.fail(attempt, "failed", "host did not return ServerHello");
      return;
    }
    const hello = frame.payload.value;
    const bulk = this.options.bulk;
    if (bulk && (hello.serverIdentity !== bulk.expectedServerIdentity || hello.connectionEpoch !== bulk.connectionEpoch)) {
      this.fail(attempt, "failed", "bulk connection was bound to a different control connection");
      return;
    }
    if (!bulk && (hello.terminalOutputWindowBytes === 0n || hello.terminalOutputWindowRecords === 0)) {
      this.fail(attempt, "failed", "host terminal output credit requires a bounded window");
      return;
    }
    attempt.hello = hello;
    this.log(`handshake.ok helper=${hello.helperVersion} build=${hello.helperBuildDigest.slice(0, 12) || "unknown"}`);
    if (bulk) {
      this.goLive(attempt);
      this.options.onConnected?.();
      return;
    }
    attempt.credit = new OutputCreditLedger({
      windowBytes: hello.terminalOutputWindowBytes,
      send: (ack) => {
        if (this.attempt !== attempt) return;
        this.send(attempt, envelope(0n, {
          case: "terminalOutputAck",
          value: create(TerminalOutputAckSchema, {
            connectionEpoch: attempt.connectionEpoch,
            cumulativeBytes: ack.cumulativeBytes,
            cumulativeRecords: ack.cumulativeRecords,
          }),
        }));
      },
    });
    attempt.phase = "subscribe";
  }

  private onSubscribeResponse(attempt: Attempt, response: Response): void {
    const hello = attempt.hello;
    if (!hello) return;
    if (!response.ok) {
      this.fail(attempt, "failed", `${response.errorCode}: ${response.displayMessage}`);
      return;
    }
    const snapshot = response.snapshot;
    if (!snapshot) {
      this.fail(attempt, "failed", "subscribe response omitted snapshot");
      return;
    }
    if (snapshot.serverIdentity !== hello.serverIdentity) {
      this.fail(attempt, "failed", "tmux server changed during handshake");
      return;
    }
    const store = this.options.store;
    store.getState().clearHostState();
    store.getState().setServerIdentity(hello.serverIdentity);
    this.applySnapshotWithAgents(snapshot);
    attempt.lastSequence = response.acceptedSequence;
    this.goLive(attempt);
    this.clearStableTimer();
    // The backoff exponent resets only after 60 s of stability (§7.2); the
    // store's `attempt` is a display value and is 0 whenever connected.
    this.stableTimer = this.timer.set(this.stableAfterMs, () => {
      this.stableTimer = undefined;
      if (this.attempt === attempt) this.reconnectAttempt = 0;
    });
    // A snapshot barrier already incorporates every ordered event at or below
    // its accepted sequence; replaying one would read as a gap. Mirrors
    // `event_follows_snapshot_barrier` in the desktop bridge.
    const buffered = attempt.buffered;
    attempt.buffered = [];
    for (const frame of buffered) {
      if (this.attempt !== attempt) return;
      if (frame.payload.case === "event" && frame.sequence <= response.acceptedSequence) {
        // Dropped, but the host already reserved its credit: acknowledge it
        // like the desktop's `forfeit_delivery_charge` does.
        const dropped = frame.payload.value;
        if (dropped.terminalDeliveryRecords > 0n || dropped.terminalDeliveryBytes > 0n) {
          attempt.credit?.charge(dropped.terminalDeliveryBytes, dropped.terminalDeliveryRecords);
        }
        continue;
      }
      this.onLiveFrame(attempt, frame);
    }
    if (this.attempt === attempt) this.options.onConnected?.();
  }

  private goLive(attempt: Attempt): void {
    attempt.phase = "live";
    if (attempt.handshakeTimer !== undefined) {
      this.timer.clear(attempt.handshakeTimer);
      attempt.handshakeTimer = undefined;
    }
    this.options.store.getState().setConnection({ state: "connected", message: undefined, attempt: 0 });
  }

  private onLiveFrame(attempt: Attempt, frame: Envelope): void {
    const payload = frame.payload;
    switch (payload.case) {
      case "response": {
        const pending = attempt.pending.get(frame.requestId);
        if (!pending) {
          this.log(`response.unmatched requestId=${frame.requestId}`);
          return;
        }
        attempt.pending.delete(frame.requestId);
        clearTimeout(pending.timer);
        this.noteAnswer(attempt);
        const response = payload.value;
        if (response.ok) pending.resolve(response);
        else pending.reject(new HostError(response.errorCode, response.displayMessage, response.voice));
        return;
      }
      case "fileStream": {
        const pending = attempt.pending.get(frame.requestId);
        if (!pending?.onFileStream) {
          this.log(`fileStream.unmatched requestId=${frame.requestId}`);
          return;
        }
        // §11.1: associate by request id and double-check the operation id.
        if (pending.operationId !== undefined && payload.value.operationId !== pending.operationId) {
          this.log(`fileStream.operation.mismatch requestId=${frame.requestId} got=${payload.value.operationId}`);
          return;
        }
        pending.onFileStream(payload.value);
        return;
      }
      case "event": {
        // §7.4 ordered events.
        if (frame.sequence !== 0n) {
          if (frame.sequence !== attempt.lastSequence + 1n) {
            this.log(`protocol.gap expected=${attempt.lastSequence + 1n} got=${frame.sequence}`);
            this.reconnectNow(attempt, "sequence gap");
            return;
          }
          attempt.lastSequence = frame.sequence;
        }
        this.onEvent(attempt, payload.value);
        return;
      }
      case "error": {
        const error = payload.value;
        if (frame.requestId === 0n) {
          this.options.onToast?.(error.displayMessage);
          if (!error.retryable) this.reconnectNow(attempt, `host error: ${error.code}`);
          return;
        }
        const pending = attempt.pending.get(frame.requestId);
        if (pending) {
          attempt.pending.delete(frame.requestId);
          clearTimeout(pending.timer);
          this.noteAnswer(attempt);
          pending.reject(new HostError(error.code, error.displayMessage));
        }
        return;
      }
      default:
        this.log(`frame.ignored ${payload.case ?? "empty"}`);
    }
  }

  private onEvent(attempt: Attempt, event: HostEvent): void {
    const store = this.options.store;
    const terminals = this.options.terminals;
    switch (event.kind) {
      case EventKind.TOPOLOGY_SNAPSHOT:
        if (event.snapshot) this.applySnapshotWithAgents(event.snapshot);
        // A notified pass that found the world unchanged answers with the
        // generation alone; ignoring it would leave guarded requests quoting a
        // stale generation and refused as stale_topology.
        else if (event.topologyGeneration > 0n) store.getState().applyTopologyAck(event.topologyGeneration);
        break;
      case EventKind.TOPOLOGY_DIRTY:
        break;
      case EventKind.RESYNC_REQUIRED:
        this.reconnectNow(attempt, "resync required");
        return;
      case EventKind.TERMINAL_SEED:
        if (event.terminal) terminals?.seed?.(event.terminal.paneId, event.terminal.data, event.terminal.generation);
        break;
      case EventKind.TERMINAL_OUTPUT:
        if (event.terminal) terminals?.output?.(event.terminal.paneId, event.terminal.data, event.terminal.generation);
        break;
      case EventKind.TERMINAL_HISTORY:
        if (event.terminal) {
          terminals?.history?.(event.terminal.paneId, event.terminal.data, event.terminal.historySize, event.terminal.historySizeKnown);
        }
        break;
      case EventKind.TERMINAL_EXIT:
        terminals?.exit?.(event.scope, event.detail);
        break;
      case EventKind.TERMINAL_FLOW_PAUSED:
      case EventKind.TERMINAL_FLOW_STALLED:
        break;
      case EventKind.TERMINAL_RESNAPSHOT_REQUIRED:
        this.requestSeed(event.scope);
        break;
      case EventKind.PANE_RESOURCE: {
        const resource = event.paneResource;
        if (resource) {
          this.log(`pane.resource ${resource.paneId} state=${resource.state} requiresSeed=${resource.requiresSeed} generation=${resource.generation}${resource.recoveryReason ? ` reason="${resource.recoveryReason}"` : ""}`);
        }
        if (resource?.requiresSeed && store.getState().focusedPaneId === resource.paneId) {
          this.requestSeed(resource.paneId);
        }
        break;
      }
      case EventKind.TERMINAL_SEED_DIAGNOSTIC:
        this.log(`seed.diagnostic ${event.scope} ${event.detail}`);
        break;
      case EventKind.AGENT_STATE: {
        if (event.agent) {
          const previousAgents = store.getState().agents;
          const previousServerIdentity = store.getState().serverIdentity;
          const transition = store.getState().applyAgentEvent(event.agent);
          logAgentTransitions(previousAgents, store.getState().agents, `event:${event.agent.reason || "update"}`, store.getState().topologyGeneration, (line) => this.log(line));
          if (transition) {
            const currentServerIdentity = store.getState().serverIdentity;
            const promotion = previousServerIdentity === currentServerIdentity
              && currentServerIdentity !== ""
              ? deriveAgentIdentityPromotion(previousAgents, event.agent, transition.next)
              : undefined;
            if (promotion) this.options.onAgentIdentityPromotion?.(promotion);
            this.options.onAgentTransition?.(transition);
          }
        }
        break;
      }
      case EventKind.ACTIVE_ROOT:
      case EventKind.DIRECTORY_SNAPSHOT:
      case EventKind.FILE_CHANGED:
        this.options.onFileEvent?.(event);
        break;
      case EventKind.VOICE_PROVISION:
      case EventKind.VOICE_REPLY:
        this.options.onVoiceEvent?.(event);
        break;
      case EventKind.TERMINAL_CLIPBOARD_WRITE:
        break;
      default:
        this.log(`event.ignored kind=${event.kind}`);
    }
    // §7.7: every event's delivery charge is acknowledged once its bytes have
    // been handed on (or discarded) — seeds, output, and the PANE_RESOURCE
    // recovery events a visibility change admits.
    if (event.terminalDeliveryRecords > 0n || event.terminalDeliveryBytes > 0n) {
      attempt.credit?.charge(event.terminalDeliveryBytes, event.terminalDeliveryRecords);
    }
  }

  private applySnapshotWithAgents(snapshot: NonNullable<HostEvent["snapshot"]>): void {
    const store = this.options.store;
    const previousServerIdentity = store.getState().serverIdentity;
    const previousAgents = store.getState().agents;
    store.getState().applySnapshot(snapshot);
    if (previousServerIdentity && previousServerIdentity !== snapshot.serverIdentity) {
      this.options.onServerIdentityChanged?.(snapshot.serverIdentity);
    }
    logAgentTransitions(previousAgents, store.getState().agents, "snapshot", snapshot.generation, (line) => this.log(line));
    if (snapshot.agents?.authoritative && this.options.onAgentTransition) {
      for (const agent of Object.values(store.getState().agents)) {
        this.options.onAgentTransition({ prev: previousAgents[agent.id], next: agent });
      }
    }
  }

  private requestSeed(paneId: string): void {
    if (!paneId.startsWith("%")) return;
    this.request(requestTerminalSeed(paneId)).catch((error: unknown) => {
      this.log(`seed.request.failed ${paneId} ${error instanceof Error ? error.message : String(error)}`);
    });
  }

  // ---- teardown and reconnect ----------------------------------------------

  /** A terminal outcome for this attempt: `failed` or `incompatible`, no retry. */
  private fail(attempt: Attempt, state: "failed" | "incompatible", message: string): void {
    attempt.outcome = { state, message };
    this.wantConnected = false;
    this.teardown(attempt, new ConnectionClosedError(message));
    this.options.store.getState().setConnection({ state, message });
    attempt.transport.close();
  }

  /** Drops the current transport and reconnects on the backoff schedule. */
  /** Any answer, even a refusal, proves the lane is alive. */
  private noteAnswer(attempt: Attempt): void {
    attempt.unansweredRequests = 0;
    attempt.lastAnswerAt = Date.now();
  }

  private reconnectNow(attempt: Attempt, reason: string): void {
    if (this.attempt !== attempt) return;
    attempt.outcome = { state: "reconnect", message: reason };
    this.teardown(attempt, new ConnectionClosedError(reason));
    attempt.transport.close();
    this.scheduleReconnect(reason);
  }

  private onClosed(attempt: Attempt, close: TransportClose): void {
    if (this.attempt !== attempt) return;
    this.teardown(attempt, new ConnectionClosedError(close.message ?? close.reason));
    if (attempt.outcome) return; // already decided by fail()/reconnectNow()
    this.afterClose(close);
  }

  /** Detaches an attempt, rejecting everything in flight. Idempotent. */
  private teardown(attempt: Attempt, error: Error): void {
    if (this.attempt === attempt) this.attempt = undefined;
    if (attempt.handshakeTimer !== undefined) {
      this.timer.clear(attempt.handshakeTimer);
      attempt.handshakeTimer = undefined;
    }
    attempt.credit?.close();
    for (const [, pending] of attempt.pending) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    attempt.pending.clear();
    this.clearStableTimer();
  }

  /** §7.2 close policy for a transport that closed on its own. */
  private afterClose(close: TransportClose): void {
    const store = this.options.store;
    if (!this.wantConnected) {
      store.getState().setConnection({ state: "idle", attempt: 0 });
      return;
    }
    if (FATAL_CLOSE_REASONS.has(close.reason) || (close.reason === "exited" && close.exitCode === HELPER_MISSING_EXIT_CODE)) {
      this.wantConnected = false;
      store.getState().setConnection({ state: "failed", message: closeMessage(close, this.options.host) });
      return;
    }
    this.scheduleReconnect(closeMessage(close, this.options.host));
  }

  private scheduleReconnect(message: string): void {
    if (!this.wantConnected) return;
    this.clearReconnectTimer();
    const n = this.reconnectAttempt;
    const delayMs = Math.min(2 ** n, this.maxBackoffMs / 1000) * 1000;
    this.reconnectAttempt = n + 1;
    this.options.store.getState().setConnection({ state: "reconnecting", attempt: n + 1, message, retryAtMs: Date.now() + delayMs });
    // `message` can contain remote SSH stderr for the user-facing strip. It is
    // deliberately excluded from copied diagnostics as untrusted free-form content.
    this.log(`reconnect.scheduled attempt=${n + 1} delayMs=${delayMs}`);
    this.reconnectTimer = this.timer.set(delayMs, () => {
      this.reconnectTimer = undefined;
      if (!this.wantConnected) return;
      this.options.store.getState().setConnection({ state: "sshConnecting" });
      void this.open();
    });
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== undefined) {
      this.timer.clear(this.reconnectTimer);
      this.reconnectTimer = undefined;
    }
  }

  private clearStableTimer(): void {
    if (this.stableTimer !== undefined) {
      this.timer.clear(this.stableTimer);
      this.stableTimer = undefined;
    }
  }

  private log(line: string): void {
    this.options.log?.(`[muxflow] ${line}`);
  }
}


function envelope(requestId: bigint, payload: Envelope["payload"]): Envelope {
  return create(EnvelopeSchema, {
    protocolMajor: PROTOCOL_MAJOR,
    requestId,
    sequence: 0n,
    streamId: 0n,
    priority: Priority.UNSPECIFIED,
    payload,
  });
}

/** §12 copy for the connection strip. */
function closeMessage(close: TransportClose, host: SavedHostRef | undefined): string {
  const where = host ? `${host.host}:${host.port}` : "the host";
  switch (close.reason) {
    case "connectFailed":
      return `Couldn't reach ${where}.`;
    case "authFailed":
      return `${host?.host ?? "The host"} rejected this phone's SSH login. Over Tailscale SSH, check the tailnet's SSH policy; otherwise add the key under Your SSH key to ~/.ssh/authorized_keys on the host.`;
    case "exited":
      if (close.exitCode === HELPER_MISSING_EXIT_CODE) {
        return "muxflow-host isn't installed on this host. Install it from the Muxflow desktop app (Settings → Connection).";
      }
      return close.message || `The helper exited${close.exitCode !== undefined ? ` (code ${close.exitCode})` : ""}.`;
    case "networkLost":
      return close.message || "Connection lost.";
    default:
      return close.message || close.reason;
  }
}
