import type { VoiceReadinessState } from "./voiceStore";

/**
 * Why the microphone is off, one phrase per condition, matching the readiness
 * card above it; empty when it can be used.
 */
export function voiceHint({ connected, gone, readiness, recorderError }: { connected: boolean; gone: boolean; readiness: VoiceReadinessState; recorderError: string | undefined }): string {
  if (!connected) return "Not connected";
  if (gone) return "This agent no longer exists";
  switch (readiness) {
    case "unknown": return "Checking voice on the host…";
    case "uvMissing": return "Needs uv on the host";
    case "provisioning": return "Setting up voice on the host…";
    case "modelMissing": return "Voice isn't set up on this host";
    default: return recorderError ? "Microphone unavailable: allow the microphone permission for Muxflow" : "";
  }
}
