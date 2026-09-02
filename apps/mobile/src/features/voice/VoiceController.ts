// One voice session with one agent (docs/mobile/voice-mode-plan.md §5.2,
// design.md §9.11), without React: the host readiness probe, the session
// registration the host needs to push replies here, hold-to-talk →
// transcribe → TERMINAL_INPUT, pushed replies → the one player, and End
// session. The session outlives the screen: `voiceRegistry` keeps the
// controller until `endSession()`; the screen `focus()`es and `blur()`s it.

import { type HostConnection } from "../../protocol/HostConnection";
import type { VoiceSpeech } from "../../protocol/gen/envelope_pb";
import { newOperationId, terminalInput, voiceProvision, voiceSession, voiceSpeak, voiceStatus, voiceTranscribe } from "../../protocol/requests";
import { utf8Encode } from "../terminal/bytes";
import { CR } from "../terminal/chips";
import type { VoiceFiles } from "./files";
import type { PlayerStatus, VoicePlayer } from "./player";
import { RECORDING_MIME, type VoiceRecorder } from "./recorder";
import { describeVoiceError, STATUS_CHANGING_CODES } from "./voiceErrors";
import { latestReply, type VoiceMessage, type VoiceStore } from "./voiceStore";

export interface VoiceControllerOptions {
  agentId: string;
  paneId: string;
  sessionId: string;
  store: VoiceStore;
  getConnection: () => HostConnection | null;
  recorder: VoiceRecorder;
  player: VoicePlayer;
  files: VoiceFiles;
  /** `AppState.currentState === "active"`: nothing auto-plays in the background (§1). */
  appInForeground: () => boolean;
  toast?: (message: string) => void;
  log?: (line: string) => void;
  now?: () => number;
  /** The host holds a 10-minute TTL on a session; the phone refreshes it every 5 (§4.3). */
  sessionRefreshMs?: number;
}

export const SESSION_REFRESH_MS = 5 * 60_000;
/** Releases shorter than this are a mis-tap, not an utterance (§5.2). */
export const MIN_UTTERANCE_MS = 300;
/** A warm host answers in ~1 s; a cold sidecar may take up to 120 s to load (§4.3). */
export const TRANSCRIBE_TIMEOUT_MS = 70_000;
export const SPEAK_TIMEOUT_MS = 60_000;
/** The ~640 MB download answers only when done; progress arrives as events. */
export const PROVISION_TIMEOUT_MS = 60 * 60_000;

export class VoiceController {
  readonly agentId: string;
  readonly paneId: string;
  readonly sessionId: string;
  private focused = false;
  private registered = false;
  private disposed = false;
  private refreshTimer: ReturnType<typeof setInterval> | undefined;
  /** The message whose file the shared player currently holds, when this controller loaded it. */
  private loadedMessageId: string | undefined;
  private lastReplyGeneration = 0n;
  private messageCounter = 0;
  private readonly unsubscribePlayer: () => void;
  private readonly now: () => number;
  private readonly sessionRefreshMs: number;

  constructor(private readonly options: VoiceControllerOptions) {
    this.agentId = options.agentId;
    this.paneId = options.paneId;
    this.sessionId = options.sessionId;
    this.now = options.now ?? Date.now;
    this.sessionRefreshMs = options.sessionRefreshMs ?? SESSION_REFRESH_MS;
    options.store.getState().ensureSession(options.agentId, options.paneId, options.sessionId, this.now());
    this.unsubscribePlayer = options.player.onStatus((status) => this.onPlayerStatus(status));
  }

  // ---- lifecycle ------------------------------------------------------------

  /**
   * The Voice screen for this agent gained focus: probe (and pre-warm) the
   * host, register the session, prepare the microphone, and play a reply
   * that arrived while nobody was listening.
   */
  focus(): void {
    if (this.disposed) return;
    this.focused = true;
    void this.refreshStatus(true);
    if (!this.registered) {
      this.registered = true;
      this.refreshTimer = setInterval(() => void this.registerSession(), this.sessionRefreshMs);
    }
    void this.registerSession();
    this.options.recorder.prepare().catch((error: unknown) => this.fail("recorder.prepare", error));
    this.playUnplayedIfListening();
  }

  /** The screen left (Back, Files); the session and its registration stay. */
  blur(): void {
    this.focused = false;
  }

  /** The app came back to the foreground while this screen is on top. */
  onAppActive(): void {
    this.playUnplayedIfListening();
  }

  /** Every reconnect is a new connection, and the host's registration is per connection (§4.3). */
  onConnected(): void {
    if (this.disposed || !this.registered) return;
    void this.registerSession();
    if (this.focused) void this.refreshStatus(true);
  }

  /** Clears the host registration, deletes the kept MP3 and forgets the session. */
  async endSession(): Promise<void> {
    if (this.disposed) return;
    const connection = this.liveConnection();
    this.dispose();
    if (!connection) return;
    try {
      await connection.request(voiceSession(""));
      this.log("session.cleared");
    } catch (error) {
      this.log(`session.clear.failed ${describe(error)}`);
    }
  }

  /** Local teardown only (the connection is gone or another host is being dialled). */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.focused = false;
    if (this.refreshTimer !== undefined) clearInterval(this.refreshTimer);
    this.refreshTimer = undefined;
    this.unsubscribePlayer();
    const store = this.options.store.getState();
    if (this.loadedMessageId !== undefined) {
      this.options.player.stop();
      store.setPlayback(undefined);
      this.loadedMessageId = undefined;
    }
    const reply = latestReply(store.sessions[this.agentId]);
    if (reply?.fileUri) this.options.files.delete(reply.fileUri);
    store.removeSession(this.agentId);
  }

  // ---- host readiness ---------------------------------------------------------

  async refreshStatus(warm = false): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) return;
    try {
      const response = await connection.request(voiceStatus(newOperationId(), warm));
      if (this.disposed) return;
      if (response.voice?.status) this.options.store.getState().setHostStatus(response.voice.status);
      this.log(`status readiness=${response.voice?.status?.readiness ?? "?"} warm=${warm}`);
    } catch (error) {
      this.fail("status", error);
    }
  }

  /** The consent dialog's confirm: start the download; progress arrives as events. */
  async provision(): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) return;
    const store = this.options.store.getState();
    // The button reads as taken before the host's first progress line lands.
    store.applyProvisionProgress({ phase: "installing_runtime", transferredBytes: 0, totalBytes: 0, error: "" });
    try {
      const response = await connection.request(voiceProvision(newOperationId()), { timeoutMs: PROVISION_TIMEOUT_MS });
      if (this.disposed) return;
      if (response.voice?.status) this.options.store.getState().setHostStatus(response.voice.status);
      this.log("provision → ok");
    } catch (error) {
      this.fail("provision", error);
      // Whatever the refusal was, the card should show the host's own view of it.
      void this.refreshStatus(false);
    }
  }

  // ---- hold to talk -----------------------------------------------------------

  /** Press-in. Any playback stops: the user is talking now. */
  beginUtterance(): void {
    if (this.disposed) return;
    const store = this.options.store.getState();
    if (store.sessions[this.agentId]?.phase !== "idle") return;
    if (this.loadedMessageId !== undefined && store.playback?.state === "playing") this.stop();
    try {
      this.options.recorder.record();
    } catch (error) {
      this.fail("record", error);
      return;
    }
    store.setPhase(this.agentId, "recording");
    this.log("utterance.begin");
  }

  /** Press-out: stop → read → VOICE_TRANSCRIBE → TERMINAL_INPUT of `transcript + CR`. */
  async endUtterance(): Promise<void> {
    if (this.disposed) return;
    const store = this.options.store.getState();
    if (store.sessions[this.agentId]?.phase !== "recording") return;
    store.setPhase(this.agentId, "transcribing");
    const { recorder, files } = this.options;
    let uri: string | null = null;
    let durationMs = 0;
    try {
      ({ uri, durationMs } = await recorder.stop());
    } catch (error) {
      this.fail("recorder.stop", error);
    }
    // Re-arm for the next press while the host works on this one.
    recorder.prepare().catch((error: unknown) => this.fail("recorder.prepare", error));
    if (!uri || durationMs < MIN_UTTERANCE_MS) {
      this.log(`utterance.discarded durationMs=${durationMs}`);
      this.setPhaseIfAlive("idle");
      return;
    }
    const startedAt = this.now();
    let text: string;
    try {
      const audio = await files.read(uri);
      files.delete(uri);
      const connection = this.liveConnection();
      if (!connection) throw new Error("Not connected.");
      const response = await connection.request(voiceTranscribe(newOperationId(), audio, RECORDING_MIME), { timeoutMs: TRANSCRIBE_TIMEOUT_MS });
      const transcript = response.voice?.transcript;
      text = transcript?.text.trim() ?? "";
      this.log(`transcribe ${audio.byteLength} bytes durationMs=${durationMs} → ${text.length} chars in ${this.now() - startedAt} ms (audio ${transcript?.audioMillis ?? 0} ms, decode ${transcript?.decodeMillis ?? 0} ms)`);
    } catch (error) {
      this.fail("transcribe", error);
      this.setPhaseIfAlive("idle");
      return;
    }
    if (this.disposed) return;
    if (!text) {
      this.options.toast?.("Didn't catch that.");
      this.setPhaseIfAlive("idle");
      return;
    }
    const live = this.options.store.getState();
    live.setPhase(this.agentId, "sending");
    live.appendMessage(this.agentId, this.message("you", text));
    try {
      const connection = this.liveConnection();
      if (!connection) throw new Error("Not connected.");
      const body = utf8Encode(text);
      const bytes = new Uint8Array(body.length + 1);
      bytes.set(body);
      bytes[body.length] = CR[0]!;
      await connection.request(terminalInput(this.paneId, bytes));
      this.log(`input ${bytes.byteLength} bytes → ok in ${this.now() - startedAt} ms`);
    } catch (error) {
      this.fail("input", error);
    }
    this.setPhaseIfAlive("idle");
  }

  // ---- replies ----------------------------------------------------------------

  /**
   * A pushed EVENT_KIND_VOICE_REPLY for this agent. `failureDetail` is set when
   * the host could not synthesize (`audio` empty): the text still shows, with
   * a Retry that calls VOICE_SPEAK.
   */
  onVoiceReply(speech: VoiceSpeech, failureDetail = ""): void {
    if (this.disposed) return;
    if (speech.stateGeneration !== 0n && speech.stateGeneration === this.lastReplyGeneration) {
      this.log(`reply.duplicate generation=${speech.stateGeneration}`);
      return;
    }
    this.lastReplyGeneration = speech.stateGeneration;
    const store = this.options.store.getState();
    const previous = latestReply(store.sessions[this.agentId]);
    if (previous?.fileUri) {
      if (this.loadedMessageId === previous.id) {
        this.options.player.stop();
        store.setPlayback(undefined);
        this.loadedMessageId = undefined;
      }
      this.options.files.delete(previous.fileUri);
    }
    const message = this.message("agent", speech.text, {
      at: speech.replyAtUnixMillis > 0n ? Number(speech.replyAtUnixMillis) : this.now(),
      truncated: speech.truncated,
      played: false,
    });
    if (speech.audio.byteLength > 0) {
      try {
        message.fileUri = this.options.files.writeReply(this.agentId, speech.audio);
      } catch (error) {
        message.audioError = describe(error);
      }
    } else {
      message.audioError = failureDetail || "The host couldn't synthesize this reply.";
    }
    store.appendReply(this.agentId, message);
    this.log(`reply ${speech.audio.byteLength} bytes text=${speech.text.length} chars${speech.truncated ? " truncated" : ""}${message.audioError ? ` audioError=${message.audioError}` : ""}`);
    this.playUnplayedIfListening();
  }

  /** The Retry on a reply whose pushed audio failed: synthesize it now and play. */
  async retrySpeak(messageId: string): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) return;
    const message = this.find(messageId);
    if (!message || message.kind !== "agent") return;
    try {
      const response = await connection.request(voiceSpeak(newOperationId(), message.text), { timeoutMs: SPEAK_TIMEOUT_MS });
      if (this.disposed) return;
      const speech = response.voice?.speech;
      if (!speech || speech.audio.byteLength === 0) throw new Error("The host returned no audio.");
      // Only the newest reply keeps a file; a retry on an older one plays and is not kept.
      const uri = this.options.files.writeReply(this.agentId, speech.audio);
      this.options.store.getState().setMessageAudio(this.agentId, messageId, uri);
      this.log(`speak.retry ${speech.audio.byteLength} bytes`);
      this.play(messageId);
    } catch (error) {
      this.fail("speak", error);
    }
  }

  // ---- playback ---------------------------------------------------------------

  play(messageId: string): void {
    if (this.disposed) return;
    const message = this.find(messageId);
    if (!message?.fileUri) return;
    const store = this.options.store.getState();
    const current = store.playback;
    if (this.loadedMessageId !== messageId || current?.messageId !== messageId) {
      this.options.player.load(message.fileUri);
      this.loadedMessageId = messageId;
      store.setPlayback({ messageId, state: "playing", positionMs: 0, durationMs: 0 });
    } else {
      store.setPlayback({ ...current, state: "playing" });
    }
    this.options.player.play();
    store.markPlayed(this.agentId, messageId);
    this.log(`play ${messageId}`);
  }

  pause(): void {
    const playback = this.ownPlayback();
    if (!playback) return;
    this.options.player.pause();
    this.options.store.getState().setPlayback({ ...playback, state: "paused" });
  }

  resume(): void {
    const playback = this.ownPlayback();
    if (!playback) return;
    this.options.player.play();
    this.options.store.getState().setPlayback({ ...playback, state: "playing" });
  }

  stop(): void {
    const playback = this.ownPlayback();
    if (!playback) return;
    this.options.player.stop();
    this.options.store.getState().setPlayback({ ...playback, state: "stopped", positionMs: 0 });
  }

  seek(positionMs: number): void {
    const playback = this.ownPlayback();
    if (!playback) return;
    const clamped = Math.max(0, playback.durationMs > 0 ? Math.min(positionMs, playback.durationMs) : positionMs);
    this.options.player.seek(clamped);
    this.options.store.getState().setPlayback({ ...playback, positionMs: clamped });
  }

  private onPlayerStatus(status: PlayerStatus): void {
    const playback = this.ownPlayback();
    if (!playback) return;
    const store = this.options.store.getState();
    if (status.finished) {
      this.options.player.stop();
      store.setPlayback({ ...playback, state: "stopped", positionMs: 0, durationMs: status.durationMs || playback.durationMs });
      return;
    }
    const state = status.playing ? "playing" : playback.state;
    const positionMs = playback.state === "stopped" && !status.playing ? 0 : status.positionMs;
    store.setPlayback({ ...playback, state, positionMs, durationMs: status.durationMs || playback.durationMs });
  }

  /** The store's playback entry when the shared player holds one of this session's replies. */
  private ownPlayback() {
    const playback = this.options.store.getState().playback;
    return playback && this.loadedMessageId !== undefined && playback.messageId === this.loadedMessageId ? playback : undefined;
  }

  /** §1: the newest unplayed reply plays when this screen is focused, in the foreground, with auto-play on. */
  private playUnplayedIfListening(): void {
    if (this.disposed || !this.focused || !this.options.appInForeground()) return;
    const store = this.options.store.getState();
    if (!store.autoPlay) return;
    const reply = latestReply(store.sessions[this.agentId]);
    if (reply && !reply.played && reply.fileUri) this.play(reply.id);
  }

  // ---- helpers ------------------------------------------------------------------

  private async registerSession(): Promise<void> {
    const connection = this.liveConnection();
    if (!connection || this.disposed) return;
    try {
      await connection.request(voiceSession(this.agentId));
      this.log("session.registered");
    } catch (error) {
      this.fail("session", error);
    }
  }

  private liveConnection(): HostConnection | null {
    const connection = this.options.getConnection();
    return connection && connection.state === "connected" ? connection : null;
  }

  private setPhaseIfAlive(phase: "idle"): void {
    if (this.disposed) return;
    this.options.store.getState().setPhase(this.agentId, phase);
  }

  private find(messageId: string): VoiceMessage | undefined {
    return this.options.store.getState().sessions[this.agentId]?.messages.find((message) => message.id === messageId);
  }

  private message(kind: VoiceMessage["kind"], text: string, overrides: Partial<VoiceMessage> = {}): VoiceMessage {
    this.messageCounter += 1;
    return {
      id: `${this.agentId}:${this.now()}:${this.messageCounter}`,
      kind,
      text,
      at: this.now(),
      truncated: false,
      fileUri: undefined,
      audioError: undefined,
      played: true,
      ...overrides,
    };
  }

  private fail(what: string, error: unknown): void {
    const message = describeVoiceError(error);
    this.log(`${what}.failed ${describe(error)}`);
    if (message === undefined) return;
    this.options.store.getState().setLastError(message);
    this.options.toast?.(message);
    const code = (error as { code?: unknown }).code;
    if (typeof code === "string" && STATUS_CHANGING_CODES.has(code) && what !== "status") void this.refreshStatus(false);
  }

  private log(line: string): void {
    this.options.log?.(`[muxflow] voice ${this.agentId} ${line}`);
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
