// The audio and file ports the controller drives (docs/mobile/voice-mode-plan.md
// §5.2). Pure types plus one constant, so `VoiceController` and the registry
// never import expo-audio or expo-file-system: those load with the screen
// (§2c), and the tests substitute fakes.

export interface VoiceRecorder {
  /** Permission, audio mode and `prepareToRecordAsync`, so press-in only calls `record()` (§2b). */
  prepare(): Promise<void>;
  record(): void;
  /** Stops and returns the file and its length; `uri` is null when nothing was captured. */
  stop(): Promise<{ uri: string | null; durationMs: number }>;
  release(): void;
}

/** `Request.voice.audioMime` for expo-audio's `.m4a` recordings. */
export const RECORDING_MIME = "audio/mp4";

export interface PlayerStatus {
  positionMs: number;
  durationMs: number;
  playing: boolean;
  /** The current source reached its end. */
  finished: boolean;
}

export interface VoicePlayer {
  /** Loads `uri` (replacing any previous source) without playing it. */
  load(uri: string): void;
  play(): void;
  pause(): void;
  /** Pauses and rewinds. */
  stop(): void;
  seek(positionMs: number): void;
  /** Playback speed for the current and every later source (1 = natural), pitch kept. */
  setRate(rate: number): void;
  onStatus(listener: (status: PlayerStatus) => void): () => void;
  release(): void;
}

export interface VoiceFiles {
  read(uri: string): Promise<Uint8Array>;
  /** Writes the reply MP3 for `agentId`, replacing any previous one, and returns its uri. */
  writeReply(agentId: string, bytes: Uint8Array): string;
  delete(uri: string): void;
}
