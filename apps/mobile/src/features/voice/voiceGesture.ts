export const VOICE_GESTURE_THRESHOLD = 64;

/** A deliberate, primarily-horizontal move locks left and cancels right. */
export function horizontalVoiceGesture(dx: number, dy: number): "lock" | "cancel" | undefined {
  if (Math.abs(dx) < VOICE_GESTURE_THRESHOLD || Math.abs(dx) <= Math.abs(dy)) return undefined;
  return dx < 0 ? "lock" : "cancel";
}
