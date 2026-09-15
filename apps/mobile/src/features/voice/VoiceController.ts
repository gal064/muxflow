// One voice session with one pane (docs/mobile/voice-mode-plan.md §5.2,
// design.md §9.11), without React: the host readiness probe, the session
// registration the host needs to push replies here, hold-to-talk →
// transcribe → TERMINAL_INPUT, pushed replies → the one player, and End
// session. The session outlives the screen: `voiceRegistry` keeps the
// controller until `endSession()`; the screen `focus()`es and `blur()`s it.

import { HostError, type HostConnection } from "../../protocol/HostConnection";
import type { VoiceSpeech } from "../../protocol/gen/envelope_pb";
import { newOperationId, terminalInput, voiceProvision, voiceSession, voiceSpeak, voiceStatus, voiceTranscribe } from "../../protocol/requests";
import { utf8Encode } from "../terminal/bytes";
import { CR } from "../terminal/chips";
import { SUBMIT_DELAY_MS } from "../terminal/TerminalController";
import type { AgentLifecycle } from "../../store/sessionStore";
import { RECORDING_MIME, type PlayerStatus, type VoiceFiles, type VoicePlayer, type VoiceRecorder } from "./audioPorts";
import type { VoiceHaptics } from "./haptics";
import type { VoiceRecorderCoordinator } from "./recorderCoordinator";
import type { VoiceTones } from "./tones";
import { describeVoiceError, STATUS_CHANGING_CODES } from "./voiceErrors";
import { latestReply, type VoiceMessage, type VoiceReadinessState, type VoiceStore } from "./voiceStore";

export interface VoiceControllerOptions {
  serverIdentity: string;
  /** Registry-assigned immutable local identity; defaults to the pane id in direct tests. */
  sessionKey?: string;
  paneId: string;
  sessionId: string;
  store: VoiceStore;
  getConnection: () => HostConnection | null;
  recorder: VoiceRecorder;
  /** Process-global ordering and listening state for the shared native recorder. */
  recorderCoordinator: VoiceRecorderCoordinator;
  player: VoicePlayer;
  files: VoiceFiles;
  /** `AppState.currentState === "active"`: nothing auto-plays in the background (§1). */
  appInForeground: () => boolean;
  /** Tactile acknowledgements for the steps the user cannot see; absent in tests that do not care. */
  haptics?: VoiceHaptics;
  /** Short tones for the same steps, all but the press (a sound there would be recorded). */
  tones?: VoiceTones;
  /** Initial reply playback speed; `setPlaybackRate` follows the preference afterwards. */
  playbackRate?: number;
  /** Initial autoplay preference; `setAutoPlay` follows it afterwards. */
  autoPlay?: boolean;
  toast?: (message: string) => void;
  log?: (line: string) => void;
  now?: () => number;
  /** The host holds a 10-minute TTL on a session; the phone refreshes it every 5 (§4.3). */
  sessionRefreshMs?: number;
  /** Test seam for `TAIL_HOLD_MS`; 0 stops the recorder the moment the press ends. */
  tailHoldMs?: number;
  /** Gap between the transcript paste and the CR that submits it (`SUBMIT_DELAY_MS`); tests pass 0. */
  submitDelayMs?: number;
  /** Rechecked after transcription and before Enter so an ordinary shell never receives a submission. */
  canSubmit?: (paneId: string) => boolean;
}

export const SESSION_REFRESH_MS = 5 * 60_000;
/** The steps between press and pane whose failure loses the utterance; each gets the error haptic. */
const UTTERANCE_STEPS = new Set(["record", "recorder.stop", "transcribe", "input"]);
/** Releases shorter than this are a mis-tap, not an utterance (§5.2). */
export const MIN_UTTERANCE_MS = 300;
/**
 * How long the recorder keeps running after the finger lifts. Android's
 * MediaRecorder drops the audio still in its encoder pipeline when stopped
 * (the phone's clips came out ~200 ms shorter than the hold), and a speaker
 * lets go on the last syllable, so an immediate stop loses the final word.
 */
export const TAIL_HOLD_MS = 350;
/** A warm host answers in ~1 s; a cold sidecar may take up to 120 s to load (§4.3). */
export const TRANSCRIBE_TIMEOUT_MS = 70_000;
export const SPEAK_TIMEOUT_MS = 60_000;
/** The 697 MiB download answers only when done; progress arrives as events. */
export const PROVISION_TIMEOUT_MS = 60 * 60_000;

/** Refusals that are really a readiness report (§4.6): the card shows them, no toast. */
const READINESS_CODES: Record<string, VoiceReadinessState> = {
  voice_uv_missing: "uvMissing",
  voice_model_missing: "modelMissing",
  voice_provisioning: "provisioning",
};

export class VoiceController {
  /** Immutable key for local messages, audio ownership, and controller state. */
  readonly sessionKey: string;
  private paneId: string;
  private sessionId: string;
  private focused = false;
  /** The screen has been opened at least once: the host should know about this session. */
  private wantRegistered = false;
  private disposed = false;
  /** Runs only after a registration succeeded; stops on a refusal that says the host is not ready. */
  private refreshTimer: ReturnType<typeof setInterval> | undefined;
  private registering = false;
  /** A `reregister()` that arrived while one was in flight runs right after it settles. */
  private registerAgain = false;
  /** The host acknowledged a registration at least once on this session's life. */
  private everRegistered = false;
  /** The message whose file the shared player currently holds, when this controller loaded it. */
  private loadedMessageId: string | undefined;
  private lastReplyGeneration = 0n;
  private messageCounter = 0;
  /** The `prepare()` in flight, so a press that lands before it settles waits for it instead of failing. */
  private arming: Promise<void> | undefined;
  /** A right-swipe or locked-screen exit is stopping and discarding the current take. */
  private canceling: Promise<void> | undefined;
  /** Stable identity for this controller's entry in the shared listening set. */
  private readonly recorderOwner = Symbol("voice-recorder-owner");
  private readonly unsubscribePlayer: () => void;
  private readonly now: () => number;
  private readonly sessionRefreshMs: number;
  private readonly tailHoldMs: number;
  private readonly submitDelayMs: number;
  private playbackRate: number;
  private autoPlay: boolean;
  /** A reply that arrived while the microphone was listening stays manual-play only. */
  private autoPlaySuppressedMessageId: string | undefined;
  /** The agent lifecycle last reported by the screen; the working acknowledgement fires on the edge into `working`. */
  private lastLifecycle: AgentLifecycle | undefined;
  /** The "you" message whose pickup by the agent was already acknowledged. */
  private workingAckedFor: string | undefined;

  constructor(private readonly options: VoiceControllerOptions) {
    this.sessionKey = options.sessionKey ?? `${options.serverIdentity}:${options.paneId}`;
    this.paneId = options.paneId;
    this.sessionId = options.sessionId;
    this.now = options.now ?? Date.now;
    this.sessionRefreshMs = options.sessionRefreshMs ?? SESSION_REFRESH_MS;
    this.tailHoldMs = options.tailHoldMs ?? TAIL_HOLD_MS;
    this.submitDelayMs = options.submitDelayMs ?? SUBMIT_DELAY_MS;
    this.playbackRate = options.playbackRate ?? 1;
    this.autoPlay = options.autoPlay ?? true;
    options.store.getState().ensureSession(this.sessionKey, options.paneId, options.sessionId, this.now());
    this.unsubscribePlayer = options.player.onStatus((status) => this.onPlayerStatus(status));
  }

  get isDisposed(): boolean {
    return this.disposed;
  }

  get target(): { paneId: string; sessionId: string } {
    return { paneId: this.paneId, sessionId: this.sessionId };
  }

  /** The agent moved panes: the next transcript types into the new one. */
  retarget(paneId: string, sessionId: string): void {
    if (this.disposed || (paneId === this.paneId && sessionId === this.sessionId)) return;
    this.paneId = paneId;
    this.sessionId = sessionId;
    this.options.store.getState().ensureSession(this.sessionKey, paneId, sessionId, this.now());
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
    this.log("screen.focus");
    this.wantRegistered = true;
    void this.refreshStatus(true);
    void this.registerSession();
    this.arm();
    this.playUnplayedIfListening();
  }

  /** The screen left (Back, Files); the session and its registration stay. */
  blur(): void {
    this.focused = false;
    this.log("screen.blur");
    // The next mount re-baselines the lifecycle: an edge that happened while
    // another screen was up is not acknowledged late, or for the wrong turn.
    this.lastLifecycle = undefined;
    // React Native can unmount the pressed control without delivering
    // `onPressOut`. Stop either kind of live take so this session cannot keep
    // the process-global recorder from the next voice screen.
    if (this.recordingPhase() !== undefined) void this.cancelUtterance();
    else this.disarm();
  }

  /** The app came back to the foreground while this screen is on top. */
  onAppActive(): void {
    if (this.focused) this.arm();
    this.playUnplayedIfListening();
  }

  /** A live take never keeps the microphone when the app leaves the foreground. */
  onAppInactive(): void {
    if (this.recordingPhase() !== undefined) void this.cancelUtterance();
    else this.disarm();
  }

  /** Every reconnect is a new connection, and the host's registration is per connection (§4.3). */
  onConnected(): void {
    if (this.disposed) return;
    this.reregister();
    if (this.focused) void this.refreshStatus(true);
  }

  /** Registers again on the current connection (a reconnect, or another session's End cleared it). */
  reregister(): void {
    if (this.disposed || !this.wantRegistered) return;
    void this.registerSession();
  }

  /** Clears the host registration, deletes the kept MP3 and forgets the session. */
  async endSession(): Promise<void> {
    if (this.disposed) return;
    const connection = this.liveConnection();
    // A registration the host may hold (acknowledged, or still in flight) is cleared; one it refused is not.
    const registered = this.everRegistered || this.registering;
    this.dispose();
    if (!connection || !registered) return;
    try {
      await connection.request(voiceSession(""));
      this.log("session.cleared");
    } catch (error) {
      this.log(`session.clear.failed ${diagnosticError(error)}`);
    }
  }

  /** Local teardown only (the connection is gone or another host is being dialled). */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.focused = false;
    this.wantRegistered = false;
    this.disarm(true);
    this.stopRefreshTimer();
    this.unsubscribePlayer();
    const store = this.options.store.getState();
    if (this.ownPlayback()) {
      this.options.player.stop();
      store.setPlayback(undefined);
    }
    this.loadedMessageId = undefined;
    const reply = latestReply(store.sessions[this.sessionKey]);
    if (reply?.fileUri) this.options.files.delete(reply.fileUri);
    store.removeSession(this.sessionKey);
  }

  // ---- host readiness ---------------------------------------------------------

  async refreshStatus(warm = false): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) {
      // Nothing to ask yet; `onConnected` asks again once the lane is live.
      this.log(`status.skipped warm=${warm} connection=${this.options.getConnection()?.state ?? "none"}`);
      return;
    }
    const operationId = newOperationId();
    this.log(`status.request operation=${operationId} warm=${warm}`);
    try {
      const response = await connection.request(voiceStatus(operationId, warm));
      if (this.disposed) return;
      if (response.voice?.status) this.options.store.getState().setHostStatus(response.voice.status);
      this.log(`status readiness=${response.voice?.status?.readiness ?? "?"} warm=${warm}`);
      // A host that became ready since the last try can take the registration now.
      if (this.options.store.getState().hostStatus.readiness === "ready" && this.refreshTimer === undefined && !this.registering) this.reregister();
    } catch (error) {
      if (this.applyReadinessRefusal(error)) return;
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
    const operationId = newOperationId();
    this.log(`provision.request operation=${operationId}`);
    try {
      const response = await connection.request(voiceProvision(operationId), { timeoutMs: PROVISION_TIMEOUT_MS });
      if (this.disposed) return;
      if (response.voice?.status) this.options.store.getState().setHostStatus(response.voice.status);
      this.log("provision → ok");
      this.reregister();
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
    if (store.sessions[this.sessionKey]?.phase !== "idle" || store.recorderError) return;
    // Whatever is playing, this session's reply or another's, yields to the voice.
    if (store.playback?.state === "playing") {
      this.options.player.stop();
      store.setPlayback({ ...store.playback, state: "stopped", positionMs: 0 });
    }
    store.setPhase(this.sessionKey, "recording");
    this.setListening(true);
    this.log(`utterance.begin recorder=${this.recorderState()}`);
    // A press that lands while the recorder is still re-arming after the
    // previous release waits for it; a release before then finds nothing
    // recorded and is discarded.
    const armed = this.arming ?? Promise.resolve();
    void armed.then(async () => {
      if (this.disposed || this.recordingPhase() === undefined) return;
      try {
        const started = await this.options.recorderCoordinator.runOwned(this.recorderOwner, () => {
          if (this.disposed || this.recordingPhase() === undefined) return;
          this.log(`recorder.start.requested actual=${this.recorderState()}`);
          this.options.recorder.record();
          this.log(`recorder.started actual=${this.recorderState()}`);
          return true;
        });
        if (!started || this.disposed || this.recordingPhase() === undefined) {
          this.setListening(false);
          this.setPhaseIfAlive("idle");
          return;
        }
        this.options.haptics?.listening();
      } catch (error) {
        this.setListening(false);
        this.fail("record", error);
        this.setPhaseIfAlive("idle");
      }
    });
  }

  /** A left swipe detaches the recording from the finger that started it. */
  lockUtterance(): void {
    if (this.recordingPhase() !== "recording") return;
    this.options.store.getState().setPhase(this.sessionKey, "recordingLocked");
    this.log("utterance.locked");
  }

  /** Stop the current take and delete it without transcription or terminal input. */
  async cancelUtterance(): Promise<void> {
    if (this.canceling) return this.canceling;
    if (this.recordingPhase() === undefined) return;
    this.options.store.getState().setPhase(this.sessionKey, "canceling");
    this.log("utterance.canceled");
    const { recorder, files } = this.options;
    this.canceling = this.options.recorderCoordinator.teardown(this.recorderOwner, async () => {
      let uri: string | null = null;
      try {
        ({ uri } = await recorder.stop());
      } catch (error) {
        this.log(`recorder.stop.failed ${diagnosticError(error)}`);
      }
      this.setListening(false);
      if (uri) files.delete(uri);
      recorder.release();
    }).then(() => undefined);
    try {
      await this.canceling;
    } finally {
      this.setListening(false);
      this.canceling = undefined;
      this.setPhaseIfAlive("idle");
      if (!this.disposed && this.focused && this.options.appInForeground()) this.arm();
    }
  }

  /** Press-out: stop → read → VOICE_TRANSCRIBE → TERMINAL_INPUT of the transcript as a paste, then a CR on its own. */
  async endUtterance(): Promise<void> {
    if (this.disposed) return;
    const store = this.options.store.getState();
    if (this.recordingPhase() === undefined) return;
    store.setPhase(this.sessionKey, "transcribing");
    const { recorder, files } = this.options;
    // The press may still be waiting for the recorder (see beginUtterance).
    await (this.arming ?? Promise.resolve()).catch(() => undefined);
    // Let the last syllable land before the encoder is stopped (TAIL_HOLD_MS).
    if (this.tailHoldMs > 0) await new Promise<void>((resolve) => setTimeout(resolve, this.tailHoldMs));
    if (this.disposed) return;
    let uri: string | null = null;
    let durationMs = 0;
    let audio: Uint8Array | undefined;
    let stopFailed = false;
    let stopFailure: unknown;
    try {
      await this.options.recorderCoordinator.teardown(this.recorderOwner, async () => {
        try {
          try {
            this.log(`recorder.stop.requested actual=${this.recorderState()}`);
            ({ uri, durationMs } = await recorder.stop());
            this.log(`recorder.stopped actual=${this.recorderState()} durationMs=${durationMs}`);
          } catch (error) {
            stopFailed = true;
            stopFailure = error;
            return;
          } finally {
            this.setListening(false);
          }
          if (!uri || durationMs < MIN_UTTERANCE_MS) return;
          try {
            // On iOS a bare prepare reuses (and truncates) the same file URL, so
            // recorder ownership lasts until these bytes have been consumed.
            audio = await files.read(uri);
          } finally {
            files.delete(uri);
          }
        } finally {
          if (this.disposed || !this.focused) recorder.release();
        }
      });
      this.setListening(false);
    } catch (error) {
      this.rearm();
      this.fail("transcribe", error);
      this.setPhaseIfAlive("idle");
      return;
    }
    if (stopFailed) this.fail("recorder.stop", stopFailure);
    if (!uri || durationMs < MIN_UTTERANCE_MS) {
      this.log(`utterance.discarded durationMs=${durationMs}`);
      this.rearm();
      this.setPhaseIfAlive("idle");
      return;
    }
    const startedAt = this.now();
    let text: string;
    try {
      // Re-arm for the next press while the host works on this one.
      this.rearm();
      const connection = this.liveConnection();
      if (!connection) throw new Error("Not connected.");
      const operationId = newOperationId();
      this.log(`transcribe.request operation=${operationId} bytes=${audio!.byteLength} durationMs=${durationMs}`);
      const response = await connection.request(voiceTranscribe(operationId, audio!, RECORDING_MIME), { timeoutMs: TRANSCRIBE_TIMEOUT_MS });
      const transcript = response.voice?.transcript;
      // The host single-lines the transcript (§4.4); a newline here would submit mid-text, so it is never trusted to.
      text = (transcript?.text ?? "").replace(/\s*[\r\n]+\s*/g, " ").trim();
      this.log(`transcribe ${audio!.byteLength} bytes durationMs=${durationMs} → ${text.length} chars in ${this.now() - startedAt} ms (audio ${transcript?.audioMillis ?? 0} ms, decode ${transcript?.decodeMillis ?? 0} ms)`);
    } catch (error) {
      this.rearm();
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
    if (this.options.canSubmit?.(this.paneId) === false) {
      this.log("utterance.discarded pane-agent-departed");
      this.setPhaseIfAlive("idle");
      return;
    }
    const live = this.options.store.getState();
    live.setPhase(this.sessionKey, "sending");
    const outgoing = this.message("you", text);
    live.appendMessage(this.sessionKey, outgoing);
    try {
      const connection = this.liveConnection();
      if (!connection) throw new Error("Not connected.");
      // The terminal Send's shape (`TerminalController.submitText`): the text as
      // one paste, which the host never coalesces with its neighbours and tmux
      // brackets when the composer asked for it, then the CR as a keystroke after
      // a gap. Claude Code and Codex both read an Enter inside a fast burst as a
      // pasted newline; on its own it submits. A refused paste sends no CR.
      const body = utf8Encode(text);
      await connection.request(terminalInput(this.paneId, body, {
        paste: true,
        voice: true,
        expectedServerIdentity: this.options.serverIdentity,
      }));
      if (this.submitDelayMs > 0) await new Promise<void>((resolve) => setTimeout(resolve, this.submitDelayMs));
      if (this.disposed || this.options.canSubmit?.(this.paneId) === false) {
        this.log("input.submit.skipped pane-agent-departed");
        this.setPhaseIfAlive("idle");
        return;
      }
      await connection.request(terminalInput(this.paneId, CR, {
        voice: true,
        expectedServerIdentity: this.options.serverIdentity,
      }));
      this.log(`submission.ack message=${outgoing.id} bytes=${body.byteLength} elapsedMs=${this.now() - startedAt}`);
      this.options.haptics?.sent();
      this.options.tones?.sent();
    } catch (error) {
      this.fail("input", error);
    }
    this.setPhaseIfAlive("idle");
  }

  /**
   * The agent's lifecycle as the screen sees it. The edge into `working`
   * while the last turn is the user's is the agent picking the utterance up:
   * one haptic per utterance, none when the agent was already working (it has
   * not reached this message yet) and none for a screen that opens onto an
   * agent mid-turn (each mount re-baselines, see `blur`). Delivered only while
   * a Voice screen for this agent is mounted: that is where lifecycles are
   * reported from, and where the user is waiting without looking.
   */
  onAgentLifecycle(lifecycle: AgentLifecycle): void {
    const previous = this.lastLifecycle;
    this.lastLifecycle = lifecycle;
    if (this.disposed || lifecycle !== "working" || previous === undefined || previous === "working") return;
    const last = this.options.store.getState().sessions[this.sessionKey]?.messages.at(-1);
    if (!last || last.kind !== "you" || this.workingAckedFor === last.id) return;
    this.workingAckedFor = last.id;
    this.log(`working.ack message=${last.id}`);
    this.options.haptics?.working();
    this.options.tones?.working();
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
    const previous = latestReply(store.sessions[this.sessionKey]);
    if (previous?.fileUri) {
      // The shared player is stopped only if it still holds *this* reply; another
      // session may have taken it since (`loadedMessageId` alone would be stale).
      if (store.playback?.messageId === previous.id) {
        this.options.player.stop();
        store.setPlayback(undefined);
      }
      if (this.loadedMessageId === previous.id) this.loadedMessageId = undefined;
      this.options.files.delete(previous.fileUri);
    }
    const message = this.message("agent", speech.displayMarkdown, {
      at: speech.replyAtUnixMillis > 0n ? Number(speech.replyAtUnixMillis) : this.now(),
      speechText: speech.speechText,
      truncated: speech.truncated,
      played: false,
    });
    this.autoPlaySuppressedMessageId = this.options.recorderCoordinator.isListening ? message.id : undefined;
    if (speech.audio.byteLength > 0) {
      try {
        message.fileUri = this.options.files.writeReply(this.sessionKey, speech.audio);
      } catch (error) {
        message.audioError = describe(error);
      }
    } else {
      message.audioError = failureDetail || "The host couldn't synthesize this reply.";
    }
    store.appendReply(this.sessionKey, message);
    this.log(`reply message=${message.id} stateGeneration=${speech.stateGeneration} audioBytes=${speech.audio.byteLength} markdownChars=${speech.displayMarkdown.length} speechChars=${speech.speechText.length} truncated=${speech.truncated} audioError=${message.audioError !== undefined}`);
    this.playUnplayedIfListening();
  }

  /** The Retry on the newest reply whose pushed audio failed: synthesize it now and play. */
  async retrySpeak(messageId: string): Promise<void> {
    const connection = this.liveConnection();
    if (!connection) return;
    const message = this.find(messageId);
    // Only the newest reply may hold the session's one file (§1).
    if (!message || message.kind !== "agent" || latestReply(this.options.store.getState().sessions[this.sessionKey])?.id !== messageId) return;
    const operationId = newOperationId();
    this.log(`speak.request operation=${operationId} message=${messageId}`);
    try {
      const response = await connection.request(voiceSpeak(operationId, message.speechText), { timeoutMs: SPEAK_TIMEOUT_MS });
      if (this.disposed) return;
      const speech = response.voice?.speech;
      if (!speech || speech.audio.byteLength === 0) throw new Error("The host returned no audio.");
      const uri = this.options.files.writeReply(this.sessionKey, speech.audio);
      this.options.store.getState().setMessageAudio(this.sessionKey, messageId, uri);
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
      this.options.player.setRate(this.playbackRate);
      this.loadedMessageId = messageId;
      store.setPlayback({ messageId, state: "playing", positionMs: 0, durationMs: 0 });
    } else {
      store.setPlayback({ ...current, state: "playing" });
    }
    this.options.player.play();
    store.markPlayed(this.sessionKey, messageId);
    this.log(`play message=${messageId}`);
  }

  /** The preference changed: the reply playing now and every later one take the new speed. */
  setPlaybackRate(rate: number): void {
    if (this.playbackRate === rate) return;
    this.playbackRate = rate;
    if (this.loadedMessageId !== undefined) this.options.player.setRate(rate);
  }

  /** The preference changed; enabling it also plays the newest reply that arrived while it was off. */
  setAutoPlay(autoPlay: boolean): void {
    if (this.autoPlay === autoPlay) return;
    this.autoPlay = autoPlay;
    if (autoPlay) this.playUnplayedIfListening();
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
    if (this.disposed || !this.focused || !this.options.appInForeground() || this.options.recorderCoordinator.isListening) return;
    if (!this.autoPlay) return;
    const store = this.options.store.getState();
    const reply = latestReply(store.sessions[this.sessionKey]);
    if (reply && !reply.played && reply.fileUri && reply.id !== this.autoPlaySuppressedMessageId) this.play(reply.id);
  }

  // ---- helpers ------------------------------------------------------------------

  /**
   * Nobody is listening: an armed recorder holds an open (empty) file in the
   * cache, so release it when this screen is no longer usable. The next active
   * focus arms again. An already captured utterance may keep transcribing.
   */
  private disarm(disposing = false): void {
    const { recorder, files } = this.options;
    // Disposed mid-hold (End, disconnect): endUtterance() will not run, so the
    // live recording is stopped here, or the microphone stays hot.
    if (disposing && (this.recordingPhase() !== undefined || this.options.recorderCoordinator.isListeningFor(this.recorderOwner))) {
      void this.options.recorderCoordinator.teardown(this.recorderOwner, async () => {
        try {
          const { uri } = await recorder.stop();
          if (uri) files.delete(uri);
        } catch (error) {
          this.log(`recorder.stop.failed ${diagnosticError(error)}`);
        } finally {
          this.setListening(false);
          recorder.release();
        }
      }).finally(() => this.setListening(false));
      return;
    }
    void this.options.recorderCoordinator.release(this.recorderOwner, () => {
      if (this.focused && this.options.appInForeground()) return false;
      return !this.options.recorderCoordinator.isListeningFor(this.recorderOwner);
    }, () => recorder.release());
  }

  /** After a release: arm again for the next press if someone is still on the screen, else let the recorder go. */
  private rearm(): void {
    if (this.focused) this.arm();
    else void this.options.recorderCoordinator.release(this.recorderOwner, () => true, () => this.options.recorder.release());
  }

  /** Prepares the recorder once per release so press-in is `record()` alone (§2b). */
  private arm(): void {
    if (this.arming || this.canceling) return;
    const store = this.options.store.getState();
    if (store.recorderError) return;
    this.log(`recorder.prepare.requested actual=${this.recorderState()}`);
    this.arming = this.options.recorderCoordinator.claim(
      this.recorderOwner,
      // A claim may wait behind another voice session. Re-check foreground
      // state when it is eventually retried so a canceled background session
      // cannot acquire and retain the shared recorder later.
      () => !this.disposed && this.focused && this.options.appInForeground(),
      () => this.options.recorder.prepare(),
    )
      .then((claimed) => {
        if (claimed && !this.disposed) {
          this.options.store.getState().setRecorderError(undefined);
          this.log(`recorder.prepared actual=${this.recorderState()}`);
        }
      })
      .catch((error: unknown) => {
        if (this.disposed) return;
        // Permission denied is a phone-side state the mic shows until it changes;
        // any other prepare failure is transient and toasts once, and the next
        // focus or release arms again.
        if ((error as { name?: unknown }).name === "RecordingPermissionDenied") {
          this.log(`recorder.prepare.denied ${diagnosticError(error)}`);
          this.options.store.getState().setRecorderError(describe(error));
        } else {
          this.fail("recorder.prepare", error);
        }
      })
      .finally(() => {
        this.arming = undefined;
      });
  }

  private recordingPhase(): "recording" | "recordingLocked" | undefined {
    const phase = this.options.store.getState().sessions[this.sessionKey]?.phase;
    return phase === "recording" || phase === "recordingLocked" ? phase : undefined;
  }

  private async registerSession(): Promise<void> {
    const connection = this.liveConnection();
    if (!connection || this.disposed || !this.wantRegistered) return;
    if (this.registering) {
      // Another session's End may have wiped this registration while ours was in flight: go again after it.
      this.registerAgain = true;
      return;
    }
    this.registering = true;
    try {
      await connection.request(voiceSession(this.paneId, this.options.serverIdentity));
      if (this.disposed) return;
      this.everRegistered = true;
      this.log("session.registered");
      this.refreshTimer ??= setInterval(() => void this.registerSession(), this.sessionRefreshMs);
      // The host just answered on this lane, so a status probe that never
      // settled (or was skipped) is not the host's fault: ask once more rather
      // than leave the card at "Checking voice on the host…".
      if (this.options.store.getState().hostStatus.readiness === "unknown") void this.refreshStatus(false);
    } catch (error) {
      // A host that is not set up cannot hold a session; the readiness card
      // says so, and the loop restarts once STATUS reports ready.
      if (this.applyReadinessRefusal(error)) {
        this.stopRefreshTimer();
        this.log(`session.refused ${diagnosticError(error)}`);
      } else if (this.focused) {
        this.fail("session", error);
      } else {
        // A refresh failing behind another screen (link drop, slow host) is not worth a toast there.
        this.log(`session.refresh.failed ${diagnosticError(error)}`);
      }
    } finally {
      this.registering = false;
      if (this.registerAgain) {
        this.registerAgain = false;
        void this.registerSession();
      }
    }
  }

  private stopRefreshTimer(): void {
    if (this.refreshTimer !== undefined) clearInterval(this.refreshTimer);
    this.refreshTimer = undefined;
  }

  /** A `voice_uv_missing` / `voice_model_missing` / `voice_provisioning` refusal becomes the readiness shown on the card. */
  private applyReadinessRefusal(error: unknown): boolean {
    if (!(error instanceof HostError)) return false;
    const readiness = READINESS_CODES[error.code];
    if (!readiness || this.disposed) return false;
    const store = this.options.store.getState();
    if (readiness !== "provisioning" || store.hostStatus.readiness !== "provisioning") store.setReadiness(readiness, error.message);
    return true;
  }

  private liveConnection(): HostConnection | null {
    const connection = this.options.getConnection();
    return connection && connection.state === "connected" ? connection : null;
  }

  private setPhaseIfAlive(phase: "idle"): void {
    if (this.disposed) return;
    this.options.store.getState().setPhase(this.sessionKey, phase);
    // The screen may have been left while the host was transcribing: the recorder re-armed then is nobody's now.
    if (!this.focused) this.disarm();
  }

  private find(messageId: string): VoiceMessage | undefined {
    return this.options.store.getState().sessions[this.sessionKey]?.messages.find((message) => message.id === messageId);
  }

  private message(kind: VoiceMessage["kind"], text: string, overrides: Partial<VoiceMessage> = {}): VoiceMessage {
    this.messageCounter += 1;
    return {
      id: `${this.sessionKey}:${this.now()}:${this.messageCounter}`,
      kind,
      displayText: text,
      speechText: text,
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
    this.log(`${what}.failed ${diagnosticError(error)}`);
    // A request that settles after End or disconnect has nobody to tell.
    if (message === undefined || this.disposed) return;
    this.options.store.getState().setLastError(message);
    this.options.toast?.(message);
    if (UTTERANCE_STEPS.has(what)) {
      this.options.haptics?.failed();
      this.options.tones?.failed();
    }
    const code = (error as { code?: unknown }).code;
    if (typeof code === "string" && STATUS_CHANGING_CODES.has(code) && what !== "status") void this.refreshStatus(false);
  }

  private recorderState(): string {
    return this.options.recorder.state?.() ?? "unknown";
  }

  private setListening(listening: boolean): void {
    this.options.recorderCoordinator.setListening(this.recorderOwner, listening);
  }

  private log(line: string): void {
    this.options.log?.(`[muxflow] voice pane=${this.paneId} session=${this.sessionId} ${line}`);
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Content-free error identity for copied diagnostics; UI still receives the friendly message. */
function diagnosticError(error: unknown): string {
  if (error instanceof HostError) {
    return `type=HostError code=${safeToken(error.code)} operation=${safeToken(error.voice?.operationId ?? "none")} retryable=${error.voice?.retryable ?? false}`;
  }
  if (error instanceof Error) return `type=Error messageChars=${error.message.length}`;
  return `type=${typeof error}`;
}

function safeToken(value: string): string {
  return value.replace(/[^A-Za-z0-9._:-]/g, "_").slice(0, 160) || "unknown";
}
