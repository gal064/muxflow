// Subtle audible acknowledgements for hold-to-talk (design.md §9.11). The
// screen is used without looking at it, so the steps the eye would confirm
// get a short, quiet tone: an up-tick when the host accepted the transcript,
// one soft note when the agent turned to it, a low down-tick when the
// utterance was lost. Three bundled WAV clips of 70–165 ms, so a tone never
// waits on the host. `VoiceTones` is the port the controller drives;
// `createExpoTones` is the expo-audio implementation, imported only by the
// Voice screen (§2c).

import { createAudioPlayer, type AudioPlayer, type AudioSource } from "expo-audio";

export interface VoiceTones {
  /** The transcript was typed into the pane and the host acknowledged the input. */
  sent(): void;
  /** The agent moved to `working` after the utterance went out. */
  working(): void;
  /** The utterance was lost: microphone, transcription or input failed. */
  failed(): void;
}

// Metro bundles these as assets; the `require`s run when this module loads,
// which is when the Voice screen does.
const CLIPS: Record<keyof VoiceTones, AudioSource> = {
  sent: require("../../../assets/tones/sent.wav") as AudioSource,
  working: require("../../../assets/tones/working.wav") as AudioSource,
  failed: require("../../../assets/tones/failed.wav") as AudioSource,
};

export function createExpoTones(): VoiceTones {
  // One player per clip, created on first use and kept: a tone is a rewind and
  // play, never a load, so it sounds the instant it is called for.
  const players = new Map<keyof VoiceTones, AudioPlayer>();
  const play = (name: keyof VoiceTones): void => {
    try {
      let player = players.get(name);
      if (!player) {
        player = createAudioPlayer(CLIPS[name]);
        players.set(name, player);
      }
      player.seekTo(0).catch(() => undefined);
      player.play();
    } catch {
      // Best-effort; the bubbles and the working indicator still tell the story.
    }
  };
  return {
    sent: () => play("sent"),
    working: () => play("working"),
    failed: () => play("failed"),
  };
}
