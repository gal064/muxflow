import { describe, expect, it } from "vitest";
import { Operation, VoiceProvider } from "./gen/envelope_pb";
import { resolveTerminalFile, voiceProvision, voiceSession, voiceSpeak, voiceStatus, voiceTranscribe } from "./requests";

describe("terminal file request builder", () => {
  it("binds the candidate to the exact pane route and host generation", () => {
    const request = resolveTerminalFile("op-file", "%7", "../README.md", "server-a", 42n, {
      sessionId: "$2",
      windowId: "@5",
      cwd: "/home/user/project/src",
    });

    expect(request.operation).toBe(Operation.RESOLVE_TERMINAL_FILE);
    expect(request.file).toMatchObject({
      operationId: "op-file",
      paneId: "%7",
      path: "../README.md",
      expectedServerIdentity: "server-a",
      expectedTopologyGeneration: 42n,
      expectedSessionId: "$2",
      expectedWindowId: "@5",
      expectedCwd: "/home/user/project/src",
    });
  });
});

// Field usage of the voice builders against docs/mobile/voice-mode-plan.md §3:
// everything rides in `Request.voice`, nothing in `scope` or `data`.
describe("voice request builders", () => {
  it("voiceStatus probes readiness and only warms when asked", () => {
    const cold = voiceStatus("op-1");
    expect(cold.operation).toBe(Operation.VOICE_STATUS);
    expect(cold.voice?.operationId).toBe("op-1");
    expect(cold.voice?.warm).toBe(false);
    expect(cold.voice?.confirmed).toBe(false);
    expect(cold.scope).toBe("");
    expect(cold.data.length).toBe(0);

    expect(voiceStatus("op-2", true).voice?.warm).toBe(true);
  });

  it("voiceProvision carries the user's consent, which the host refuses without", () => {
    const request = voiceProvision("op-3");
    expect(request.operation).toBe(Operation.VOICE_PROVISION);
    expect(request.voice?.operationId).toBe("op-3");
    expect(request.voice?.confirmed).toBe(true);
    expect(request.voice?.warm).toBe(false);
  });

  it("voiceTranscribe puts the whole recording and its mime in the voice payload, not Request.data", () => {
    const audio = new Uint8Array([0, 0, 0, 0x1c, 0x66, 0x74, 0x79, 0x70, 0xff]);
    const request = voiceTranscribe("op-4", audio, "audio/mp4");
    expect(request.operation).toBe(Operation.VOICE_TRANSCRIBE);
    expect(request.voice?.operationId).toBe("op-4");
    expect(request.voice?.audio).toBe(audio);
    expect(request.voice?.audioMime).toBe("audio/mp4");
    expect(request.voice?.languageHint).toBe("");
    expect(request.data.length).toBe(0);
  });

  it("voiceSpeak sends the text, an optional voice, and leaves the provider to the host default", () => {
    const request = voiceSpeak("op-5", "Done. Two files changed.");
    expect(request.operation).toBe(Operation.VOICE_SPEAK);
    expect(request.voice?.operationId).toBe("op-5");
    expect(request.voice?.text).toBe("Done. Two files changed.");
    expect(request.voice?.voice).toBe("");
    expect(request.voice?.provider).toBe(VoiceProvider.UNSPECIFIED);

    expect(voiceSpeak("op-6", "Hi", "en-GB-SoniaNeural").voice?.voice).toBe("en-GB-SoniaNeural");
  });

  it("voiceSession registers an agent id and clears it with the empty string", () => {
    const register = voiceSession("codex:native-7");
    expect(register.operation).toBe(Operation.VOICE_SESSION);
    expect(register.voice?.agentId).toBe("codex:native-7");
    expect(register.voice?.operationId).toBe("");

    expect(voiceSession("").voice?.agentId).toBe("");
  });
});
