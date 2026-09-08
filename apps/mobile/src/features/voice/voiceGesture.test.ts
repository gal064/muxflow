import { describe, expect, it } from "vitest";

import {
  horizontalVoiceGesture,
  previewVoiceGesture,
  VOICE_GESTURE_PREVIEW_THRESHOLD,
  VOICE_GESTURE_THRESHOLD,
} from "./voiceGesture";

describe("horizontalVoiceGesture", () => {
  it("locks left, cancels right, and ignores short or primarily vertical movement", () => {
    expect(horizontalVoiceGesture(-VOICE_GESTURE_THRESHOLD, 0)).toBe("lock");
    expect(horizontalVoiceGesture(VOICE_GESTURE_THRESHOLD, 0)).toBe("cancel");
    expect(horizontalVoiceGesture(VOICE_GESTURE_THRESHOLD - 1, 0)).toBeUndefined();
    expect(horizontalVoiceGesture(-80, 81)).toBeUndefined();
  });

  it("previews the destination before the action threshold without changing the action", () => {
    expect(previewVoiceGesture(-VOICE_GESTURE_PREVIEW_THRESHOLD, 0)).toBe("lock");
    expect(previewVoiceGesture(VOICE_GESTURE_PREVIEW_THRESHOLD, 0)).toBe("cancel");
    expect(previewVoiceGesture(VOICE_GESTURE_PREVIEW_THRESHOLD - 1, 0)).toBeUndefined();
    expect(previewVoiceGesture(-40, 41)).toBeUndefined();
    expect(horizontalVoiceGesture(VOICE_GESTURE_THRESHOLD - 1, 0)).toBeUndefined();
  });
});
