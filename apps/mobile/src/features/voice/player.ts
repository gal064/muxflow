// The one reply player (docs/mobile/voice-mode-plan.md §5.2 "Playback").
// `VoicePlayer` is what the controller drives; `createExpoPlayer` wraps a
// single expo-audio `AudioPlayer` whose source is swapped per reply.

import { createAudioPlayer, type AudioPlayer, type AudioStatus } from "expo-audio";

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
  onStatus(listener: (status: PlayerStatus) => void): () => void;
  release(): void;
}

/** Position updates at 4 Hz: enough for a thin bar, far from a re-render storm (§2b). */
export const PLAYER_UPDATE_INTERVAL_MS = 250;

export function createExpoPlayer(): VoicePlayer {
  let player: AudioPlayer | undefined;
  const listeners = new Set<(status: PlayerStatus) => void>();
  const ensure = (): AudioPlayer => {
    if (player) return player;
    player = createAudioPlayer(null, { updateInterval: PLAYER_UPDATE_INTERVAL_MS });
    player.addListener("playbackStatusUpdate", (status: AudioStatus) => {
      const mapped: PlayerStatus = {
        positionMs: Math.round(status.currentTime * 1000),
        durationMs: Number.isFinite(status.duration) ? Math.round(status.duration * 1000) : 0,
        playing: status.playing,
        finished: status.didJustFinish,
      };
      for (const listener of listeners) listener(mapped);
    });
    return player;
  };
  return {
    load(uri) {
      ensure().replace({ uri });
    },
    play() {
      ensure().play();
    },
    pause() {
      player?.pause();
    },
    stop() {
      if (!player) return;
      player.pause();
      void player.seekTo(0);
    },
    seek(positionMs) {
      void player?.seekTo(Math.max(0, positionMs) / 1000);
    },
    onStatus(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    release() {
      player?.remove();
      player = undefined;
      listeners.clear();
    },
  };
}
