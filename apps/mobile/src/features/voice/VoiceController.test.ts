import { create } from "@bufbuild/protobuf";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HostError } from "../../protocol/HostConnection";
import { Operation, VoiceReadiness, VoiceResponseSchema, VoiceSpeechSchema, type VoiceSpeech } from "../../protocol/gen/envelope_pb";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, speechResponse, statusResponse, transcriptResponse } from "./testing";
import { MIN_UTTERANCE_MS, SESSION_REFRESH_MS, VoiceController } from "./VoiceController";
import { createVoiceStore, latestReply } from "./voiceStore";

const settle = () => vi.advanceTimersByTimeAsync(0);
const MP3 = new TextEncoder().encode("mp3-bytes");

function harness(agentId = "agent-a", paneId = "%3") {
  const store = createVoiceStore();
  const connection = new FakeConnection();
  connection.answer(Operation.VOICE_STATUS, () => statusResponse(VoiceReadiness.READY));
  connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("list the files in this directory"));
  const recorder = new FakeRecorder();
  const player = new FakePlayer();
  const files = new FakeFiles();
  let foreground = true;
  const toasts: string[] = [];
  const controller = new VoiceController({
    agentId,
    paneId,
    sessionId: "$1",
    store,
    getConnection: () => connection.asHostConnection(),
    recorder,
    player,
    files,
    appInForeground: () => foreground,
    toast: (message) => toasts.push(message),
    now: () => Date.now(),
  });
  return { store, connection, recorder, player, files, controller, toasts, setForeground: (value: boolean) => { foreground = value; } };
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

    // Hold / release → transcript → TERMINAL_INPUT of utf8(text) + CR.
    h.controller.beginUtterance();
    expect(h.store.getState().sessions["agent-a"]?.phase).toBe("recording");
    expect(h.recorder.recording).toBe(true);
    await h.controller.endUtterance();
    const transcribe = h.connection.of(Operation.VOICE_TRANSCRIBE);
    expect(transcribe).toHaveLength(1);
    expect(transcribe[0]!.voice?.audioMime).toBe("audio/mp4");
    expect(new TextDecoder().decode(transcribe[0]!.voice?.audio)).toBe("aac-bytes");
    const input = h.connection.of(Operation.TERMINAL_INPUT);
    expect(input).toHaveLength(1);
    expect(input[0]!.scope).toBe("%3");
    expect(new TextDecoder().decode(input[0]!.data)).toBe("list the files in this directory\r");
    expect(input[0]!.data.at(-1)).toBe(0x0d);
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
    expect(latest.fileUri).toBe("file:///cache/voice/agent-a-1.mp3");
    expect(latest.played).toBe(true);
    expect(h.player.loaded).toBe("file:///cache/voice/agent-a-1.mp3");
    expect(h.player.calls.filter((c) => c === "play")).toHaveLength(1);
    expect(h.store.getState().playback).toMatchObject({ messageId: latest.id, state: "playing" });

    // Background: the second reply replaces the first file, stops its playback, and stays unplayed.
    h.setForeground(false);
    h.controller.onVoiceReply(reply("agent-a", "Second answer."));
    expect(h.files.deleted).toContain("file:///cache/voice/agent-a-1.mp3");
    expect(h.player.calls.at(-1)).toBe("stop");
    latest = latestReply(h.store.getState().sessions["agent-a"])!;
    expect(latest.text).toBe("Second answer.");
    expect(latest.played).toBe(false);
    expect(latest.fileUri).toBe("file:///cache/voice/agent-a-2.mp3");
    expect(h.store.getState().playback).toBeUndefined();
    const first = h.store.getState().sessions["agent-a"]!.messages[1]!;
    expect(first.fileUri).toBeUndefined(); // older reply stays as text

    // Foreground again with the screen still on top: the unplayed reply plays.
    h.setForeground(true);
    h.controller.onAppActive();
    expect(h.player.loaded).toBe("file:///cache/voice/agent-a-2.mp3");
    expect(latestReply(h.store.getState().sessions["agent-a"])!.played).toBe(true);

    // End session: host registration cleared with "", file deleted, session forgotten.
    await h.controller.endSession();
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["agent-a", ""]);
    expect(h.files.deleted).toContain("file:///cache/voice/agent-a-2.mp3");
    expect(h.store.getState().sessions["agent-a"]).toBeUndefined();
    expect(h.store.getState().playback).toBeUndefined();
  });

  it("sends nothing for an empty transcript and toasts", async () => {
    const h = harness();
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("   "));
    h.controller.focus();
    await settle();
    h.controller.beginUtterance();
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
    expect(latestReply(a.store.getState().sessions["agent-b"])!.fileUri).toBe("file:///cache/voice/agent-b-2.mp3");
    // B taking the player takes the playback slot; A's controls stop applying.
    b.play(latestReply(a.store.getState().sessions["agent-b"])!.id);
    expect(a.player.loaded).toBe("file:///cache/voice/agent-b-2.mp3");
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
    expect(retried.fileUri).toBe("file:///cache/voice/agent-a-1.mp3");
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
    await h.controller.endUtterance();
    await settle();
    expect(h.toasts).toContain("Voice isn't set up on this host yet.");
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
