// Tactile acknowledgements for hold-to-talk (design.md §9.11). The screen is
// used without looking at it, so each step the user cannot see gets a
// distinct pattern: a press that started recording, a transcript the host
// accepted into the pane, the agent picking it up, and a lost utterance.
// `VoiceHaptics` is the port the controller drives; `createExpoHaptics` is the
// expo-haptics implementation, imported only by the Voice screen (§2c).

import * as Haptics from "expo-haptics";

export interface VoiceHaptics {
  /** The recorder is capturing: the press registered. */
  listening(): void;
  /** The transcript was typed into the pane and the host acknowledged the input. */
  sent(): void;
  /** The agent moved to `working` after the utterance went out. */
  working(): void;
  /** The utterance was lost: microphone, transcription or input failed. */
  failed(): void;
}

export function createExpoHaptics(): VoiceHaptics {
  // A phone without a vibrator (or with haptics off) rejects; the acknowledgement is best-effort.
  const fire = (effect: Promise<void>): void => {
    effect.catch(() => undefined);
  };
  return {
    listening: () => fire(Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Medium)),
    sent: () => fire(Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light)),
    working: () => fire(Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success)),
    failed: () => fire(Haptics.notificationAsync(Haptics.NotificationFeedbackType.Error)),
  };
}
