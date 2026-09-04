import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostError } from "../../protocol/HostConnection";
import { Operation, ResponseSchema, VoiceReadiness, VoiceResponseSchema, VoiceSpeechSchema, type VoiceSpeech } from "../../protocol/gen/envelope_pb";
import type { VoiceHaptics } from "./haptics";
import type { VoiceTones } from "./tones";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, speechResponse, statusResponse, transcriptResponse } from "./testing";
import { MIN_UTTERANCE_MS, SESSION_REFRESH_MS, VoiceController } from "./VoiceController";
import { createVoiceStore, latestReply } from "./voiceStore";

const settle = () => vi.advanceTimersByTimeAsync(0);
const MP3 = new TextEncoder().encode("mp3-bytes");

class FakeHaptics implements VoiceHaptics {
  readonly calls: string[] = [];
  listening() { this.calls.push("listening"); }
  sent() { this.calls.push("sent"); }
  working() { this.calls.push("working"); }
  failed() { this.calls.push("failed"); }
}

class FakeTones implements VoiceTones {
  readonly calls: string[] = [];
  sent() { this.calls.push("sent"); }
  working() { this.calls.push("working"); }
  failed() { this.calls.push("failed"); }
}

function harness(agentId = "agent-a", paneId = "%3", submitDelayMs = 0) {
  const haptics = new FakeHaptics();
  const tones = new FakeTones();
  const store = createVoiceStore();
  const connection = new FakeConnection();
  connection.answer(Operation.VOICE_STATUS, () => statusResponse(VoiceReadiness.READY));
  connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("list the files in this directory"));
  const recorder = new FakeRecorder();
  const player = new FakePlayer();
  const files = new FakeFiles();
  let foreground = true;
  let canSubmit = true;
  const toasts: string[] = [];
  const controller = new VoiceController({
    tailHoldMs: 0,
    submitDelayMs,
    agentId,
    paneId,
    sessionId: "$1",
    store,
    getConnection: () => connection.asHostConnection(),
    recorder,
    player,
    files,
    appInForeground: () => foreground,
    canSubmit: () => canSubmit,
    haptics,
    tones,
    toast: (message) => toasts.push(message),
    now: () => Date.now(),
  });
  return {
    store,
    connection,
    recorder,
    player,
    files,
    haptics,
    tones,
    controller,
    toasts,
    setForeground: (value: boolean) => { foreground = value; },
    setCanSubmit: (value: boolean) => { canSubmit = value; },
  };
}

function reply(agentId: string, text: string, audio: Uint8Array = MP3, generation = 0n): VoiceSpeech {
  return create(VoiceSpeechSchema, { agentId, text, audio, audioMime: "audio/mpeg", stateGeneration: generation, replyAtUnixMillis: 1_700_000_000_000n });
}

describe("VoiceController", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("drives a full session: status, registration, utterance, input bytes, pushed replies, background, focus, End", async () => {
    const h = harness();
    // Open the screen: STATUS with warm, SESSION register, recorder prepared.
    h.controller.focus();
    await settle();
    const status = h.connection.of(Operation.VOICE_STATUS);
    expect(status).toHaveLength(1);
    expect(status[0]!.voice?.warm).toBe(true);
    expect(h.store.getState().hostStatus.readiness).toBe("ready");
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["agent-a"]);
    expect(h.recorder.prepared).toBe(1);

    // Hold / release → transcript → TERMINAL_INPUT of utf8(text) as a paste, then a CR keystroke.
    h.controller.beginUtterance();
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("recording");
    await settle();
    expect(h.recorder.recording).toBe(true);
    await h.controller.endUtterance();
    const transcribe = h.connection.of(Operation.VOICE_TRANSCRIBE);
    expect(transcribe).toHaveLength(1);
    expect(transcribe[0]!.voice?.audioMime).toBe("audio/mp4");
    expect(new TextDecoder().decode(transcribe[0]!.voice?.audio)).toBe("aac-bytes");
    const input = h.connection.of(Operation.TERMINAL_INPUT);
    expect(input).toHaveLength(2);
    expect(input.map((r) => r.scope)).toEqual(["%3", "%3"]);
    expect(new TextDecoder().decode(input[0]!.data)).toBe("list the files in this directory");
    expect(input[0]!.terminalInputPaste).toBe(true);
    expect(Array.from(input[1]!.data)).toEqual([0x0d]);
    expect(input[1]!.terminalInputPaste).toBe(false);
    const session = h.store.getState().sessions["agent-a"]!;
    expect(session.phase).toBe("idle");
    expect(session.messages.map((m) => [m.kind, m.text])).toEqual([["you", "list the files in this directory"]]);
    // The recording was consumed and the recorder re-armed for the next press.
    expect(h.files.deleted).toContain("file:///cache/rec-1.m4a");
    expect(h.recorder.prepared).toBe(2);

    // A pushed reply while focused and in the foreground: written, appended, auto-played once, marked played.
    h.controller.onVoiceReply(reply("agent-a", "Here are the files."));
    let latest = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(latest.text).toBe("Here are the files.");
    expect(latest.fileUri).toBe("file:///cache/voice/agent-a.mp3");
    expect(latest.played).toBe(true);
    expect(h.player.loaded).toBe("file:///cache/voice/agent-a.mp3");
    expect(h.player.calls.filter((c) => c === "play")).toHaveLength(1);
    expect(h.store.getState().playback).toMatchObject({ messageId: latest.id, state: "playing" });

    // Background: the second reply replaces the first file, stops its playback, and stays unplayed.
    h.setForeground(false);
    h.controller.onVoiceReply(reply("agent-a", "Second answer."));
    expect(h.files.deleted).toContain("file:///cache/voice/agent-a.mp3");
    expect(h.player.calls.at(-1)).toBe("stop");
    latest = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(latest.text).toBe("Second answer.");
    expect(latest.played).toBe(false);
    expect(latest.fileUri).toBe("file:///cache/voice/agent-a.mp3");
    expect(h.store.getState().playback).toBeUndefined();
    const first = h.store.getState().sessions["agent-a"]!.messages[1]!;
    expect(first.fileUri).toBeUndefined(); // older reply stays as text

    // Foreground again with the screen still on top: the unplayed reply plays.
    h.setForeground(true);
    h.controller.onAppActive();
    expect(h.player.loaded).toBe("file:///cache/voice/agent-a.mp3");
    expect(latestReply(h.store.getState().sessions["agent-a"])!.played).toBe(true);

    // End session: host registration cleared with "", file deleted, session forgotten.
    await h.controller.endSession();
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["agent-a", ""]);
    expect(h.files.deleted).toContain("file:///cache/voice/agent-a.mp3");
    expect(h.store.getState().sessions["agent-a"]).toBeUndefined();
    expect(h.store.getState().playback).toBeUndefined();
  });

  it("does not submit a transcript after the agent is confirmed gone", async () => {
    const h = harness();
    let answer: ((response: ReturnType<typeof transcriptResponse>) => void) | undefined;
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => new Promise((resolve) => { answer = resolve; }));
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle();
    const utterance = h.controller.endUtterance();
    await settle();

    h.setCanSubmit(false);
    answer!(transcriptResponse("do not send this"));
    await utterance;

    expect(h.connection.of(Operation.TERMINAL_INPUT)).toHaveLength(0);
    expect(h.store.getState().sessions["agent-a"]?.messages).toEqual([]);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
  });

  it("does not press Enter when the agent departs after the transcript paste", async () => {
    const h = harness("agent-a", "%3", 100);
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle();
    const utterance = h.controller.endUtterance();
    await settle();
    expect(h.connection.of(Operation.TERMINAL_INPUT)).toHaveLength(1);

    h.setCanSubmit(false);
    await vi.advanceTimersByTimeAsync(100);
    await utterance;

    expect(h.connection.of(Operation.TERMINAL_INPUT)).toHaveLength(1);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
  });

  it("sends nothing for an empty transcript and toasts", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("   "));
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle(); // the record() runs once the recorder is armed
    await h.controller.endUtterance();
    expect(h.connection.of(Operation.TERMINAL_INPUT)).toHaveLength(0);
    expect(h.toasts).toContain("Didn't catch that.");
    expect(h.store.getState().sessions["agent-a"]?.messages).toEqual([]);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
  });

  it("discards a release shorter than the minimum utterance without asking the host", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.recorder.nextDurationMs = MIN_UTTERANCE_MS - 1;
    h.controller.beginUtterance();
    await settle(); // the record() runs once the recorder is armed
    await h.controller.endUtterance();
    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(0);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
  });

  it("a reply while the screen is unfocused is stored unplayed and plays on focus", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.blur();
    h.controller.onVoiceReply(reply("agent-a", "While you were away."));
    expect(latestReply(h.store.getState().sessions["agent-a"])!.played).toBe(false);
    expect(h.player.calls).not.toContain("play");
    h.controller.focus();
    await settle();
    expect(h.player.calls).toContain("play");
    expect(latestReply(h.store.getState().sessions["agent-a"])!.played).toBe(true);
  });

  it("auto-play off keeps the reply unplayed until tapped", async () => {
    const h = harness();
    h.store.getState().setAutoPlay(false);
    h.controller.focus();
    await settle();
    h.controller.onVoiceReply(reply("agent-a", "Quiet."));
    const message = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(message.played).toBe(false);
    h.controller.play(message.id);
    expect(h.player.playing).toBe(true);
    expect(latestReply(h.store.getState().sessions["agent-a"])!.played).toBe(true);
  });

  it("two sessions receive their own replies and share the one player", async () => {
    const a = harness("agent-a", "%3");
    const b = new VoiceController({
    tailHoldMs: 0,
    submitDelayMs: 0,
      agentId: "agent-b",
      paneId: "%4",
      sessionId: "$1",
      store: a.store,
      getConnection: () => a.connection.asHostConnection(),
      recorder: a.recorder,
      player: a.player,
      files: a.files,
      appInForeground: () => true,
    });
    a.controller.focus();
    await settle();
    a.controller.onVoiceReply(reply("agent-a", "For A."));
    b.onVoiceReply(reply("agent-b", "For B."));
    expect(a.store.getState().sessions["agent-a"]!.messages.map((m) => m.text)).toEqual(["For A."]);
    expect(a.store.getState().sessions["agent-b"]!.messages.map((m) => m.text)).toEqual(["For B."]);
    // Only the focused one auto-played; B's stays unplayed with its own file.
    expect(latestReply(a.store.getState().sessions["agent-a"])!.played).toBe(true);
    expect(latestReply(a.store.getState().sessions["agent-b"])!.played).toBe(false);
    expect(latestReply(a.store.getState().sessions["agent-b"])!.fileUri).toBe("file:///cache/voice/agent-b.mp3");
    // B taking the player takes the playback slot; A's controls stop applying.
    b.play(latestReply(a.store.getState().sessions["agent-b"])!.id);
    expect(a.player.loaded).toBe("file:///cache/voice/agent-b.mp3");
    a.controller.pause();
    expect(a.store.getState().playback?.state).toBe("playing");
    b.pause();
    expect(a.store.getState().playback?.state).toBe("paused");
    b.dispose();
  });

  it("re-sends the registration every refresh interval and after a reconnect; only End clears it", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS);
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(2);
    h.controller.blur(); // leaving the screen keeps the session
    h.controller.onConnected();
    await settle();
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(3);
    expect(h.connection.of(Operation.VOICE_SESSION).every((r) => r.voice?.agentId === "agent-a")).toBe(true);
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS);
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(4);
    await h.controller.endSession();
    expect(h.connection.of(Operation.VOICE_SESSION).at(-1)?.voice?.agentId).toBe("");
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS * 2);
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(5);
  });

  it("a new utterance stops playback; pause, resume, seek and status ticks update playback", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.onVoiceReply(reply("agent-a", "Long answer."));
    const id = latestReply(h.store.getState().sessions["agent-a"])!.id;
    h.player.emit({ positionMs: 1_000, durationMs: 4_000, playing: true });
    expect(h.store.getState().playback).toEqual({ messageId: id, state: "playing", positionMs: 1_000, durationMs: 4_000 });
    h.controller.pause();
    expect(h.store.getState().playback?.state).toBe("paused");
    h.player.emit({ positionMs: 1_000, playing: false });
    expect(h.store.getState().playback?.state).toBe("paused");
    h.controller.seek(2_500);
    expect(h.player.positionMs).toBe(2_500);
    expect(h.store.getState().playback?.positionMs).toBe(2_500);
    h.controller.seek(9_000);
    expect(h.store.getState().playback?.positionMs).toBe(4_000);
    h.controller.resume();
    expect(h.store.getState().playback?.state).toBe("playing");
    h.player.emit({ positionMs: 4_000, finished: true });
    expect(h.store.getState().playback).toMatchObject({ state: "stopped", positionMs: 0 });
    h.controller.resume();
    expect(h.store.getState().playback?.state).toBe("playing");
    h.controller.beginUtterance();
    expect(h.player.playing).toBe(false);
    expect(h.store.getState().playback?.state).toBe("stopped");
  });

  it("a reply with empty audio keeps the text with an error; Retry speaks it and plays", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_SPEAK, () => speechResponse(MP3, "Spoken."));
    h.controller.focus();
    await settle();
    h.controller.onVoiceReply(reply("agent-a", "Spoken.", new Uint8Array(0)), "edge-tts: network unreachable");
    const message = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(message.fileUri).toBeUndefined();
    expect(message.audioError).toBe("edge-tts: network unreachable");
    expect(message.played).toBe(false);
    expect(h.player.calls).toEqual([]);
    await h.controller.retrySpeak(message.id);
    expect(h.connection.of(Operation.VOICE_SPEAK)[0]!.voice?.text).toBe("Spoken.");
    const retried = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(retried.fileUri).toBe("file:///cache/voice/agent-a.mp3");
    expect(retried.audioError).toBeUndefined();
    expect(retried.played).toBe(true);
    expect(h.player.playing).toBe(true);
  });

  it("a duplicate push (same non-zero generation) is ignored", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.onVoiceReply(reply("agent-a", "Once.", MP3, 7n));
    h.controller.onVoiceReply(reply("agent-a", "Once.", MP3, 7n));
    expect(h.store.getState().sessions["agent-a"]!.messages).toHaveLength(1);
  });

  it("maps host refusals to toasts and re-probes status after a readiness-changing code", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => {
      throw new HostError("voice_model_missing", "model missing", create(VoiceResponseSchema, { operationId: "x", retryable: false }));
    });
    h.controller.focus();
    await settle();
    h.connection.answer(Operation.VOICE_STATUS, () => statusResponse(VoiceReadiness.MODEL_MISSING));
    h.controller.beginUtterance();
    await settle(); // the record() runs once the recorder is armed
    await h.controller.endUtterance();
    await settle();
    expect(h.toasts).toContain("Voice isn't set up on this host.");
    expect(h.connection.of(Operation.VOICE_STATUS)).toHaveLength(2);
    expect(h.store.getState().hostStatus.readiness).toBe("modelMissing");
    expect(h.connection.of(Operation.TERMINAL_INPUT)).toHaveLength(0);
  });

  it("provision sends consent, shows an optimistic phase, and takes the READY status from the answer", async () => {
    const h = harness();
    let resolve: ((value: ReturnType<typeof statusResponse>) => void) | undefined;
    h.connection.answer(Operation.VOICE_PROVISION, () => new Promise((r) => { resolve = r; }));
    const pending = h.controller.provision();
    expect(h.connection.of(Operation.VOICE_PROVISION)[0]!.voice?.confirmed).toBe(true);
    expect(h.store.getState().hostStatus.readiness).toBe("provisioning");
    h.store.getState().applyProvisionProgress({ phase: "downloading", transferredBytes: 100, totalBytes: 1000, error: "" });
    expect(h.store.getState().hostStatus.provision).toMatchObject({ phase: "downloading", transferredBytes: 100 });
    resolve!(statusResponse(VoiceReadiness.READY));
    await pending;
    expect(h.store.getState().hostStatus.readiness).toBe("ready");
    expect(h.store.getState().hostStatus.provision).toBeUndefined();
  });
});

describe("VoiceController against a host that is not set up (review round 1)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const refuse = (code: string, message: string) => () => {
    throw new HostError(code, message, create(VoiceResponseSchema, { operationId: "x", retryable: false }));
  };

  it("a voice_model_missing refusal of STATUS and SESSION becomes the card's readiness, with no toast and no refresh loop", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_STATUS, refuse("voice_model_missing", "model not provisioned"));
    h.connection.answer(Operation.VOICE_SESSION, refuse("voice_model_missing", "model not provisioned"));
    h.controller.focus();
    await settle();
    expect(h.store.getState().hostStatus).toMatchObject({ readiness: "modelMissing", detail: "model not provisioned" });
    expect(h.toasts).toEqual([]);
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS * 2);
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(1);
    expect(h.toasts).toEqual([]);
    // uv missing maps too, and a reconnect does not toast either.
    h.connection.answer(Operation.VOICE_STATUS, refuse("voice_uv_missing", "install uv: curl ..."));
    h.connection.answer(Operation.VOICE_SESSION, refuse("voice_uv_missing", "install uv: curl ..."));
    h.controller.onConnected();
    await settle();
    expect(h.store.getState().hostStatus).toMatchObject({ readiness: "uvMissing", detail: "install uv: curl ..." });
    expect(h.toasts).toEqual([]);
    // End on a never-registered session sends no clear.
    await h.controller.endSession();
    expect(h.connection.of(Operation.VOICE_SESSION).every((r) => r.voice?.agentId === "agent-a")).toBe(true);
  });

  it("registers as soon as a later STATUS reports ready, then keeps the refresh loop", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_STATUS, refuse("voice_model_missing", "missing"));
    h.connection.answer(Operation.VOICE_SESSION, refuse("voice_model_missing", "missing"));
    h.controller.focus();
    await settle();
    h.connection.answer(Operation.VOICE_STATUS, () => statusResponse(VoiceReadiness.READY));
    h.connection.answer(Operation.VOICE_SESSION, () => create(ResponseSchema, { ok: true }));
    await h.controller.refreshStatus();
    await settle();
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS);
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(3);
  });

  it("a denied microphone permission disables recording with a phone-side state instead of a toast per press", async () => {
    const h = harness();
    const denied = new Error("Microphone permission was denied.");
    denied.name = "RecordingPermissionDenied";
    h.recorder.prepare = async () => { throw denied; };
    h.controller.focus();
    await settle();
    expect(h.store.getState().recorderError).toBe("Microphone permission was denied.");
    h.controller.beginUtterance();
    await settle();
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
    expect(h.recorder.recording).toBe(false);
    expect(h.toasts).toEqual([]);
  });

  it("a press that lands while the recorder is still re-arming waits for it", async () => {
    const h = harness();
    let armed: (() => void) | undefined;
    h.recorder.prepare = () => new Promise((resolve) => { armed = () => { h.recorder.prepared += 1; resolve(); }; });
    h.controller.focus();
    h.controller.beginUtterance();
    await settle();
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("recording");
    expect(h.recorder.recording).toBe(false);
    armed!();
    await settle();
    expect(h.recorder.recording).toBe(true);
    expect(h.toasts).toEqual([]);
  });

  it("talking stops another session's playback; ending a session leaves another's playback alone", async () => {
    const a = harness("agent-a", "%3");
    const b = new VoiceController({
    tailHoldMs: 0,
    submitDelayMs: 0,
      agentId: "agent-b",
      paneId: "%4",
      sessionId: "$1",
      store: a.store,
      getConnection: () => a.connection.asHostConnection(),
      recorder: a.recorder,
      player: a.player,
      files: a.files,
      appInForeground: () => true,
    });
    a.controller.focus();
    await settle();
    b.onVoiceReply(reply("agent-b", "For B."));
    b.play(latestReply(a.store.getState().sessions["agent-b"])!.id);
    expect(a.player.playing).toBe(true);
    a.controller.beginUtterance();
    expect(a.player.playing).toBe(false);
    expect(a.store.getState().playback?.state).toBe("stopped");
    await a.controller.endUtterance();
    b.resume();
    expect(a.player.playing).toBe(true);
    await a.controller.endSession();
    expect(a.player.playing).toBe(true);
    expect(a.store.getState().playback?.state).toBe("playing");
    b.dispose();
    expect(a.player.playing).toBe(false);
    expect(a.store.getState().playback).toBeUndefined();
  });
});

describe("VoiceController shared resources and lifecycle (review round 2)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function pair() {
    const a = harness("agent-a", "%3");
    const b = new VoiceController({
    tailHoldMs: 0,
    submitDelayMs: 0,
      agentId: "agent-b",
      paneId: "%4",
      sessionId: "$1",
      store: a.store,
      getConnection: () => a.connection.asHostConnection(),
      recorder: a.recorder,
      player: a.player,
      files: a.files,
      appInForeground: () => true,
    });
    return { ...a, b };
  }

  it("a new reply for A does not stop B's playback when B took the player after A", async () => {
    const h = pair();
    h.controller.focus();
    await settle();
    h.controller.onVoiceReply(reply("agent-a", "A one."));
    h.controller.blur();
    h.b.onVoiceReply(reply("agent-b", "B one."));
    h.b.play(latestReply(h.store.getState().sessions["agent-b"])!.id);
    expect(h.player.loaded).toBe("file:///cache/voice/agent-b.mp3");
    h.controller.onVoiceReply(reply("agent-a", "A two."));
    expect(h.player.playing).toBe(true);
    expect(h.store.getState().playback?.state).toBe("playing");
    expect(h.files.deleted).toContain("file:///cache/voice/agent-a.mp3");
    h.b.dispose();
  });

  it("a release before the recorder ever started is discarded without a host round trip", async () => {
    const h = harness();
    let armed: (() => void) | undefined;
    h.recorder.prepare = () => new Promise((resolve) => { armed = () => { h.recorder.prepared += 1; resolve(); }; });
    h.controller.focus();
    h.controller.beginUtterance();
    const released = h.controller.endUtterance();
    armed!();
    await released;
    expect(h.recorder.recording).toBe(false);
    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(0);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
    expect(h.toasts).toEqual([]);
  });

  it("a re-register that lands while one is in flight runs after it instead of being dropped", async () => {
    const h = harness();
    let release: (() => void) | undefined;
    h.connection.answer(Operation.VOICE_SESSION, () => new Promise((resolve) => { release = () => resolve(create(ResponseSchema, { ok: true })); }));
    h.controller.focus();
    await settle();
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(1);
    h.controller.reregister(); // e.g. another session's End cleared the connection
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(1);
    release!();
    await settle();
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(2);
  });

  it("End clears a registration that is still in flight; a refresh failure behind another screen only logs", async () => {
    const h = harness();
    // The first registration never answers; the clear that End sends does.
    let calls = 0;
    h.connection.answer(Operation.VOICE_SESSION, () => (calls++ === 0 ? new Promise(() => undefined) : create(ResponseSchema, { ok: true })));
    h.controller.focus();
    await settle();
    await h.controller.endSession();
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["agent-a", ""]);

    const g = harness();
    g.controller.focus();
    await settle();
    g.controller.blur();
    g.connection.answer(Operation.VOICE_SESSION, () => { throw new Error("connection closed"); });
    await vi.advanceTimersByTimeAsync(SESSION_REFRESH_MS);
    expect(g.toasts).toEqual([]);
    g.controller.focus();
    await settle();
    expect(g.toasts).toEqual(["connection closed"]);
  });

  it("retarget types the next transcript into the agent's new pane; a multi-line transcript is single-lined", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("first line\nsecond line\r\n"));
    h.controller.focus();
    await settle();
    h.controller.retarget("%9", "$2");
    expect(h.store.getState().sessions["agent-a"]).toMatchObject({ paneId: "%9", sessionId: "$2" });
    h.controller.beginUtterance();
    await settle(); // the record() runs once the recorder is armed
    await h.controller.endUtterance();
    const input = h.connection.of(Operation.TERMINAL_INPUT);
    expect(input.map((r) => r.scope)).toEqual(["%9", "%9"]);
    expect(new TextDecoder().decode(input[0]!.data)).toBe("first line second line");
    expect(Array.from(input[1]!.data)).toEqual([0x0d]);
  });
});

describe("VoiceController recorder edges (review round 3)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("reads the recording before re-arming the recorder", async () => {
    const h = harness();
    const order: string[] = [];
    const prepare = h.recorder.prepare.bind(h.recorder);
    h.recorder.prepare = async () => { order.push("prepare"); await prepare(); };
    const read = h.files.read.bind(h.files);
    h.files.read = async (uri) => { order.push("read"); return read(uri); };
    h.controller.focus();
    await settle();
    order.length = 0;
    h.controller.beginUtterance();
    await settle();
    await h.controller.endUtterance();
    expect(order).toEqual(["read", "prepare"]);
  });

  it("a transient prepare failure toasts once and does not disable the mic; a denied permission does", async () => {
    const h = harness();
    let attempts = 0;
    const prepare = h.recorder.prepare.bind(h.recorder);
    h.recorder.prepare = async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("AudioRecorderPrepareException: busy");
      await prepare();
    };
    h.controller.focus();
    await settle();
    expect(h.toasts).toEqual(["AudioRecorderPrepareException: busy"]);
    expect(h.store.getState().recorderError).toBeUndefined();
    h.controller.blur();
    h.controller.focus();
    await settle();
    expect(h.recorder.prepared).toBe(1);
    const denied = new Error("Microphone permission was denied.");
    denied.name = "RecordingPermissionDenied";
    h.recorder.prepare = async () => { throw denied; };
    h.controller.beginUtterance();
    await settle();
    await h.controller.endUtterance();
    await settle();
    expect(h.store.getState().recorderError).toBe("Microphone permission was denied.");
    expect(h.toasts).toHaveLength(1);
  });
});

describe("VoiceController recording hygiene (integration QA)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("deletes the recording after a refused transcription and after a read that fails", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => {
      throw new HostError("voice_audio_undecodable", "bad audio", create(VoiceResponseSchema, { operationId: "x", retryable: false }));
    });
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle();
    await h.controller.endUtterance();
    expect(h.files.deleted).toEqual(["file:///cache/rec-1.m4a"]);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");

    h.recorder.nextUri = "file:///cache/rec-2.m4a"; // not in the fake store: read throws
    h.controller.beginUtterance();
    await settle();
    await h.controller.endUtterance();
    expect(h.files.deleted).toEqual(["file:///cache/rec-1.m4a", "file:///cache/rec-2.m4a"]);
    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(1);
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("idle");
  });

  it("leaving the screen releases an idle recorder; the next focus arms it again; End releases too", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    expect(h.recorder.prepared).toBe(1);
    h.controller.blur();
    await settle();
    expect(h.recorder.released).toBe(1);
    h.controller.focus();
    await settle();
    expect(h.recorder.prepared).toBe(1);
    // Mid-utterance the recorder is not released from under the controller.
    h.controller.beginUtterance();
    await settle();
    h.controller.blur();
    await settle();
    expect(h.recorder.released).toBe(1);
    await h.controller.endUtterance();
    await settle();
    expect(h.recorder.released).toBe(2);
    h.controller.focus();
    await settle();
    await h.controller.endSession();
    await settle();
    expect(h.recorder.released).toBe(3);
  });
});

describe("VoiceController recorder release races (QA fix review)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("a session disposed mid-hold stops the live recording, deletes it and releases the recorder", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle();
    expect(h.recorder.recording).toBe(true);
    h.controller.dispose();
    await settle();
    expect(h.recorder.recording).toBe(false);
    expect(h.recorder.released).toBe(1);
    expect(h.files.deleted).toContain("file:///cache/rec-1.m4a");
    await h.controller.endUtterance(); // the lift after End is a no-op
    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(0);
  });

  it("leaving during the transcription releases the recorder once the utterance settles", async () => {
    const h = harness();
    let answer: ((response: ReturnType<typeof transcriptResponse>) => void) | undefined;
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => new Promise((resolve) => { answer = resolve; }));
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
    await settle();
    const released = h.controller.endUtterance();
    await settle();
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("transcribing");
    expect(h.recorder.prepared).toBe(2); // re-armed while still focused
    h.controller.blur();
    await settle();
    expect(h.recorder.released).toBe(0); // not from under a transcription
    answer!(transcriptResponse("hello"));
    await released;
    await settle();
    expect(h.recorder.released).toBe(1);
    expect(h.recorder.prepared).toBe(0);
  });
});

describe("VoiceController acknowledgements and playback speed", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  async function speak(h: ReturnType<typeof harness>) {
    h.controller.beginUtterance();
    await settle();
    await h.controller.endUtterance();
  }

  it("buzzes when recording starts and again when the host accepts the transcript", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    await speak(h);
    expect(h.haptics.calls).toEqual(["listening", "sent"]);
    // The tone follows the same steps, except the press: a sound there would be recorded.
    expect(h.tones.calls).toEqual(["sent"]);
  });

  it("gives the error pattern when the input is refused, and the empty transcript no pattern at all", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.connection.answer(Operation.TERMINAL_INPUT, () => {
      throw new HostError("pane_not_found", "pane gone", create(VoiceResponseSchema, { operationId: "x", retryable: false }));
    });
    await speak(h);
    expect(h.haptics.calls).toEqual(["listening", "failed"]);
    expect(h.tones.calls).toEqual(["failed"]);

    h.haptics.calls.length = 0;
    h.files.files.set("file:///cache/rec-1.m4a", new TextEncoder().encode("aac-bytes")); // the first read consumed it
    h.connection.answer(Operation.TERMINAL_INPUT, () => create(ResponseSchema, { ok: true }));
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("   "));
    await speak(h);
    expect(h.haptics.calls).toEqual(["listening"]);
  });

  it("acknowledges the agent picking the utterance up exactly once, on the edge into working", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    // The screen reports the lifecycle it opened on: no buzz for an agent already mid-turn.
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual([]);
    h.controller.onAgentLifecycle("idle");
    await speak(h);
    h.haptics.calls.length = 0;
    h.controller.onAgentLifecycle("working");
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual(["working"]);
    expect(h.tones.calls).toEqual(["sent", "working"]);
    // Idle and back to working without a new utterance: the reply is what is awaited, not another pickup.
    h.controller.onAgentLifecycle("idle");
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual(["working"]);
    // A reply lands: the next turn is the agent's, so a later working edge is not for us.
    h.controller.onVoiceReply(reply("agent-a", "Done."));
    h.controller.onAgentLifecycle("idle");
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual(["working"]);
  });

  it("no pickup buzz when the agent was already working as the utterance went out", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.onAgentLifecycle("idle");
    h.controller.onAgentLifecycle("working");
    await speak(h);
    h.haptics.calls.length = 0;
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual([]);
    // It finishes the earlier turn and starts ours.
    h.controller.onAgentLifecycle("idle");
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual(["working"]);
  });

  it("re-baselines the lifecycle on blur so a re-opened screen does not acknowledge a stale edge", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.onAgentLifecycle("idle");
    await speak(h);
    h.haptics.calls.length = 0;
    h.controller.blur();
    // While the Terminal screen was up the agent went working for its own reasons; the Voice screen reopens onto it.
    h.controller.focus();
    await settle();
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual([]);
    // A fresh edge seen by this mount still counts.
    h.controller.onAgentLifecycle("idle");
    h.controller.onAgentLifecycle("working");
    expect(h.haptics.calls).toEqual(["working"]);
  });

  it("gives the error pattern when the recorder fails to stop", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.recorder.stop = async () => {
      throw new Error("MediaRecorder stop failed");
    };
    await speak(h);
    expect(h.haptics.calls).toEqual(["listening", "failed"]);
    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(0);
  });

  it("applies the playback speed to each loaded reply and live to the one playing", async () => {
    const h = harness();
    h.controller.focus();
    await settle();
    h.controller.setPlaybackRate(1.5);
    h.controller.onVoiceReply(reply("agent-a", "First."));
    expect(h.player.calls.filter((call) => call.startsWith("rate"))).toEqual(["rate 1.5"]);
    expect(h.player.rate).toBe(1.5);
    h.controller.setPlaybackRate(2);
    expect(h.player.rate).toBe(2);
    // The same speed again is not re-applied.
    h.controller.setPlaybackRate(2);
    expect(h.player.calls.filter((call) => call.startsWith("rate"))).toEqual(["rate 1.5", "rate 2"]);
    h.controller.onVoiceReply(reply("agent-a", "Second.", MP3, 2n));
    expect(h.player.calls.filter((call) => call.startsWith("rate"))).toEqual(["rate 1.5", "rate 2", "rate 2"]);
  });
});
