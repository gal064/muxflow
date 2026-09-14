import { describe, expect, it } from "vitest";
import { HostError } from "../../protocol/HostConnection";
import { formatBytes, formatClock, provisionFraction, provisionPhaseLabel } from "./format";
import { describeVoiceError } from "./voiceErrors";

describe("voice copy helpers", () => {
  it("formats clocks and sizes", () => {
    expect(formatClock(0)).toBe("0:00");
    expect(formatClock(61_400)).toBe("1:01");
    expect(formatBytes(731_357_568)).toBe("697 MB");
    expect(formatBytes(671_088_640)).toBe("640 MB");
    expect(formatBytes(1536)).toBe("2 KB");
    expect(provisionFraction(5, 0)).toBeUndefined();
    expect(provisionFraction(5, 10)).toBe(0.5);
    expect(provisionFraction(50, 10)).toBe(1);
    expect(provisionPhaseLabel("downloading")).toBe("Downloading the speech model…");
    expect(provisionPhaseLabel("???")).toBe("Setting up voice…");
  });

  it("maps §4.6 codes to phone copy and keeps the host message otherwise", () => {
    expect(describeVoiceError(new HostError("cancelled", "cancelled"))).toBeUndefined();
    expect(describeVoiceError(new HostError("voice_uv_missing", "uv not found"))).toBe("uv is not installed on this host.");
    expect(describeVoiceError(new HostError("voice_audio_too_long", "audio exceeds 120 s"))).toBe("That recording couldn't be used: audio exceeds 120 s");
    expect(describeVoiceError(new HostError("voice_sidecar_failed", "crashed; see /tmp/sidecar.log"))).toBe("The voice engine failed: crashed; see /tmp/sidecar.log");
    expect(describeVoiceError(new HostError("voice_busy", ""))).toBe("Voice is busy on the host. Try again in a moment.");
    expect(describeVoiceError(new HostError("something_else", "host says so"))).toBe("host says so");
    expect(describeVoiceError(new Error("Not connected."))).toBe("Not connected.");
  });
});
