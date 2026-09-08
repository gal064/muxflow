export const VOICE_GESTURE_THRESHOLD = 64;
export const VOICE_GESTURE_PREVIEW_THRESHOLD = 12;

export type VoiceGestureDirection = "lock" | "cancel";

function horizontalDirection(dx: number, dy: number, threshold: number): VoiceGestureDirection | undefined {
  if (Math.abs(dx) < threshold || Math.abs(dx) <= Math.abs(dy)) return undefined;
  return dx < 0 ? "lock" : "cancel";
}

/** Reveal the matching destination after a small, deliberate horizontal move. */
export function previewVoiceGesture(dx: number, dy: number): VoiceGestureDirection | undefined {
  return horizontalDirection(dx, dy, VOICE_GESTURE_PREVIEW_THRESHOLD);
}

/** A deliberate, primarily-horizontal move locks left and cancels right. */
export function horizontalVoiceGesture(dx: number, dy: number): VoiceGestureDirection | undefined {
  return horizontalDirection(dx, dy, VOICE_GESTURE_THRESHOLD);
}
