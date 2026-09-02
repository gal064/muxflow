// Toast copy for the host's voice refusals (docs/mobile/voice-mode-plan.md
// §4.6). The host's `displayMessage` is the fallback; the table only rewrites
// codes whose host wording is aimed at a log rather than a phone screen.

import { HostError } from "../../protocol/HostConnection";

/** Codes after which the readiness card has changed and STATUS is worth re-asking. */
export const STATUS_CHANGING_CODES: ReadonlySet<string> = new Set([
  "voice_uv_missing",
  "voice_model_missing",
  "voice_provisioning",
  "voice_provision_failed",
]);

export function describeVoiceError(error: unknown): string | undefined {
  if (!(error instanceof HostError)) return error instanceof Error ? error.message : String(error);
  const detail = error.message ? `: ${error.message}` : "";
  switch (error.code) {
    case "cancelled":
      return undefined;
    case "voice_uv_missing":
      return "uv is not installed on this host.";
    case "voice_model_missing":
      return "Voice isn't set up on this host.";
    case "voice_consent_required":
      return "Voice setup needs your confirmation.";
    case "voice_provisioning":
      return "Voice is still being set up on the host.";
    case "voice_provision_failed":
      return `Voice setup failed${detail}`;
    case "voice_sidecar_failed":
      return `The voice engine failed${detail}`;
    case "voice_sidecar_timeout":
      return "The voice engine took too long to start. Try again.";
    case "voice_network_unavailable":
      return "The host can't reach the speech service.";
    case "voice_busy":
      return "Voice is busy on the host. Try again in a moment.";
    case "voice_too_many_sessions":
      return "Too many voice sessions on this host.";
    case "voice_nothing_to_say":
      return "Nothing to say for that reply.";
    default:
      if (error.code.startsWith("voice_audio_")) return `That recording couldn't be used${detail}`;
      return error.message || error.code;
  }
}
