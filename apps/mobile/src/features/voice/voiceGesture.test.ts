import { describe, expect, it } from "vitest";

import { horizontalVoiceGesture, VOICE_GESTURE_THRESHOLD } from "./voiceGesture";

describe("horizontalVoiceGesture", () => {
  it("locks left, cancels right, and ignores short or primarily vertical movement", () => {
    expect(horizontalVoiceGesture(-VOICE_GESTURE_THRESHOLD, 0)).toBe("lock");
    expect(horizontalVoiceGesture(VOICE_GESTURE_THRESHOLD, 0)).toBe("cancel");
    expect(horizontalVoiceGesture(VOICE_GESTURE_THRESHOLD - 1, 0)).toBeUndefined();
    expect(horizontalVoiceGesture(-80, 81)).toBeUndefined();
  });
});
