// Voice-mode state that outlives one screen (docs/mobile/voice-mode-plan.md
// §5.2, design.md §9.11). Sessions are keyed by an immutable local session key
// (initially the agent id), while `VoiceSession.agentId` follows an
// authoritative identity promotion. Each lives in memory until `endSession`,
// which is the only thing that forgets it. Nothing is persisted across restarts.
//
// One player app-wide: `playback` describes whichever message is loaded in it.
// Only the newest agent reply of a session keeps its MP3 (`fileUri`); older
// replies stay as text.

import { createStore, type StoreApi } from "zustand/vanilla";
import { VoiceReadiness, type VoiceProvisionProgress, type VoiceStatus } from "../../protocol/gen/envelope_pb";
import type { Agent } from "../../store/sessionStore";

export type VoiceReadinessState = "unknown" | "uvMissing" | "modelMissing" | "provisioning" | "ready";

export interface ProvisionProgress {
  /** installing_runtime | downloading | verifying | ready | failed */
  phase: string;
  transferredBytes: number;
  /** 0 while unknown. */
  totalBytes: number;
  error: string;
}

export interface VoiceHostStatus {
  readiness: VoiceReadinessState;
  /** Install hint when `uvMissing`; the last sidecar error otherwise. */
  detail: string;
  uvPath: string;
  modelDownloadBytes: number;
  sidecarRunning: boolean;
  provision: ProvisionProgress | undefined;
}

export type VoicePhase = "idle" | "recording" | "recordingLocked" | "canceling" | "transcribing" | "sending";

export interface VoiceMessage {
  id: string;
  kind: "you" | "agent";
  /** Original reply Markdown (plain transcript text for the user's turn). */
  displayText: string;
  /** Flattened, capped text used only for synthesis and replay. */
  speechText: string;
  /** Epoch ms. */
  at: number;
  /** The host cut the spoken text (the rest of the reply is on screen). */
  truncated: boolean;
  /** Present on the session's newest agent reply while its MP3 is kept. */
  fileUri: string | undefined;
  /** Set when the host could not synthesize this reply; a Retry offers VOICE_SPEAK. */
  audioError: string | undefined;
  /** An agent reply that has been heard (auto-played or tapped). Always true for "you". */
  played: boolean;
}

export interface VoiceSession {
  agentId: string;
  paneId: string;
  sessionId: string;
  startedAt: number;
  messages: VoiceMessage[];
  phase: VoicePhase;
}

export type PlaybackState = "playing" | "paused" | "stopped";

export interface Playback {
  messageId: string;
  state: PlaybackState;
  positionMs: number;
  /** 0 until the player reports it. */
  durationMs: number;
}

export interface VoiceState {
  hostStatus: VoiceHostStatus;
  sessions: Record<string, VoiceSession>;
  playback: Playback | undefined;
  lastError: string | undefined;
  /** Why the microphone cannot be used on this phone (permission denied); disables the mic with a hint. */
  recorderError: string | undefined;
}

export interface VoiceActions {
  setHostStatus(status: VoiceStatus): void;
  /** A refusal that says what the host lacks (`voice_uv_missing` / `voice_model_missing`) is a status too. */
  setReadiness(readiness: VoiceReadinessState, detail: string): void;
  /** One EVENT_KIND_VOICE_PROVISION line; `ready` / `failed` also move `readiness`. */
  applyProvisionProgress(progress: ProvisionProgress): void;
  /** Creates the session when absent; an existing one keeps its history. */
  ensureSession(sessionKey: string, paneId: string, sessionId: string, now: number, agentId?: string): void;
  /** Retargets only the remote identity and route; all local session state stays put. */
  promoteSession(sessionKey: string, agent: Agent): void;
  setPhase(sessionKey: string, phase: VoicePhase): void;
  appendMessage(sessionKey: string, message: VoiceMessage): void;
  /**
   * The next agent reply: the previous agent message loses its `fileUri` (the
   * caller deletes the file) and the new one is appended. Returns the uri that
   * was dropped, if any.
   */
  appendReply(sessionKey: string, message: VoiceMessage): string | undefined;
  markPlayed(sessionKey: string, messageId: string): void;
  /** A retried VOICE_SPEAK filled in the audio. */
  setMessageAudio(sessionKey: string, messageId: string, fileUri: string): void;
  setPlayback(playback: Playback | undefined): void;
  removeSession(sessionKey: string): void;
  setLastError(message: string | undefined): void;
  setRecorderError(message: string | undefined): void;
  /** Another host (or none): what we knew about the previous one no longer applies. */
  resetHostStatus(): void;
}

export type VoiceStore = StoreApi<VoiceState & VoiceActions>;

export const UNKNOWN_HOST_STATUS: VoiceHostStatus = {
  readiness: "unknown",
  detail: "",
  uvPath: "",
  modelDownloadBytes: 0,
  sidecarRunning: false,
  provision: undefined,
};

export function initialVoiceState(): VoiceState {
  return { hostStatus: UNKNOWN_HOST_STATUS, sessions: {}, playback: undefined, lastError: undefined, recorderError: undefined };
}

export function createVoiceStore(): VoiceStore {
  return createStore<VoiceState & VoiceActions>((set, get) => {
    function updateSession(sessionKey: string, update: (session: VoiceSession) => VoiceSession): void {
      const session = get().sessions[sessionKey];
      if (!session) return;
      const next = update(session);
      if (next !== session) set({ sessions: { ...get().sessions, [sessionKey]: next } });
    }

    function updateMessage(sessionKey: string, messageId: string, update: (message: VoiceMessage) => VoiceMessage): void {
      updateSession(sessionKey, (session) => {
        const index = session.messages.findIndex((message) => message.id === messageId);
        const existing = session.messages[index];
        if (!existing) return session;
        const next = update(existing);
        if (next === existing) return session;
        const messages = [...session.messages];
        messages[index] = next;
        return { ...session, messages };
      });
    }

    return {
      ...initialVoiceState(),

      setHostStatus(status) {
        set({ hostStatus: hostStatusFromProto(status) });
      },

      setReadiness(readiness, detail) {
        set({ hostStatus: { ...get().hostStatus, readiness, detail, provision: undefined } });
      },

      applyProvisionProgress(provision) {
        const current = get().hostStatus;
        const readiness: VoiceReadinessState = provision.phase === "ready"
          ? "ready"
          : provision.phase === "failed"
            ? "modelMissing"
            : "provisioning";
        set({ hostStatus: { ...current, readiness, provision: provision.phase === "ready" ? undefined : provision } });
      },

      ensureSession(sessionKey, paneId, sessionId, now, agentId = sessionKey) {
        const existing = get().sessions[sessionKey];
        if (existing) {
          if (existing.paneId === paneId && existing.sessionId === sessionId) return;
          set({ sessions: { ...get().sessions, [sessionKey]: { ...existing, paneId, sessionId } } });
          return;
        }
        set({ sessions: { ...get().sessions, [sessionKey]: { agentId, paneId, sessionId, startedAt: now, messages: [], phase: "idle" } } });
      },

      promoteSession(sessionKey, agent) {
        updateSession(sessionKey, (session) => {
          const { paneId, sessionId } = agent.route;
          if (session.agentId === agent.id && session.paneId === paneId && session.sessionId === sessionId) return session;
          return { ...session, agentId: agent.id, paneId, sessionId };
        });
      },

      setPhase(agentId, phase) {
        updateSession(agentId, (session) => (session.phase === phase ? session : { ...session, phase }));
      },

      appendMessage(agentId, message) {
        updateSession(agentId, (session) => ({ ...session, messages: [...session.messages, message] }));
      },

      appendReply(agentId, message) {
        let dropped: string | undefined;
        updateSession(agentId, (session) => {
          const messages = session.messages.map((existing) => {
            if (existing.kind !== "agent" || existing.fileUri === undefined) return existing;
            dropped = existing.fileUri;
            return { ...existing, fileUri: undefined };
          });
          messages.push(message);
          return { ...session, messages };
        });
        return dropped;
      },

      markPlayed(agentId, messageId) {
        updateMessage(agentId, messageId, (message) => (message.played ? message : { ...message, played: true }));
      },

      setMessageAudio(agentId, messageId, fileUri) {
        updateMessage(agentId, messageId, (message) => ({ ...message, fileUri, audioError: undefined }));
      },

      setPlayback(playback) {
        set({ playback });
      },

      removeSession(sessionKey) {
        const sessions = { ...get().sessions };
        delete sessions[sessionKey];
        const playback = get().playback;
        const playingHere = playback && get().sessions[sessionKey]?.messages.some((message) => message.id === playback.messageId);
        set({ sessions, ...(playingHere ? { playback: undefined } : {}) });
      },

      setLastError(message) {
        set({ lastError: message });
      },

      setRecorderError(message) {
        set({ recorderError: message });
      },

      resetHostStatus() {
        set({ hostStatus: UNKNOWN_HOST_STATUS });
      },
    };
  });
}

/** The app-wide instance. Tests create their own with `createVoiceStore()`. */
export const voiceStore: VoiceStore = createVoiceStore();

export function readinessFromProto(value: VoiceReadiness): VoiceReadinessState {
  switch (value) {
    case VoiceReadiness.UV_MISSING:
      return "uvMissing";
    case VoiceReadiness.MODEL_MISSING:
      return "modelMissing";
    case VoiceReadiness.PROVISIONING:
      return "provisioning";
    case VoiceReadiness.READY:
      return "ready";
    default:
      return "unknown";
  }
}

export function hostStatusFromProto(status: VoiceStatus): VoiceHostStatus {
  return {
    readiness: readinessFromProto(status.readiness),
    detail: status.detail,
    uvPath: status.uvPath,
    modelDownloadBytes: Number(status.modelDownloadBytes),
    sidecarRunning: status.sidecarRunning,
    provision: status.provision ? provisionFromProto(status.provision) : undefined,
  };
}

export function provisionFromProto(progress: VoiceProvisionProgress): ProvisionProgress {
  return {
    phase: progress.phase,
    transferredBytes: Number(progress.transferredBytes),
    totalBytes: Number(progress.totalBytes),
    error: progress.error,
  };
}

/** The session's newest agent reply, the one that may carry audio. */
export function latestReply(session: VoiceSession | undefined): VoiceMessage | undefined {
  if (!session) return undefined;
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (message?.kind === "agent") return message;
  }
  return undefined;
}

/**
 * An utterance has been typed into the pane and no reply has come back yet:
 * the last turn is the user's. With the agent's lifecycle `working` this is
 * when the conversation shows the working indicator (design.md §9.11).
 */
export function awaitingReply(session: VoiceSession | undefined): boolean {
  return session?.messages.at(-1)?.kind === "you";
}
