import { VOICE_PLAYBACK_RATES, type VoicePlaybackRate } from "../../store/prefsStore";

/** The next rate in the one-button 1× → 1.5× → 2× → 1× cycle. */
export function nextPlaybackRate(rate: VoicePlaybackRate): VoicePlaybackRate {
  const index = VOICE_PLAYBACK_RATES.indexOf(rate);
  return VOICE_PLAYBACK_RATES[(index + 1) % VOICE_PLAYBACK_RATES.length]!;
}

export function formatRate(rate: VoicePlaybackRate): string {
  return `${rate}×`;
}
