// Voice mode inside a chat drives the Voice screen's controller with two
// differences: a transcript is sent as a chat message, and replies are read
// aloud with VOICE_SPEAK instead of being pushed by the host.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HostError } from "../../protocol/HostConnection";
import { Operation, VoiceReadiness } from "../../protocol/gen/envelope_pb";
import { VoiceRecorderCoordinator } from "../voice/recorderCoordinator";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, speechResponse, statusResponse, transcriptResponse } from "../voice/testing";
import { VoiceController } from "../voice/VoiceController";
import { createVoiceStore, latestReply } from "../voice/voiceStore";

function harness() {
  const connection = new FakeConnection();
  connection.answer(Operation.VOICE_STATUS, () => statusResponse(VoiceReadiness.READY));
  connection.answer(Operation.VOICE_TRANSCRIBE, () => transcriptResponse("add the fixture and run the tests"));
  connection.answer(Operation.VOICE_SPEAK, (request) => speechResponse(new TextEncoder().encode("mp3"), request.voice!.text));
  const store = createVoiceStore();
  const player = new FakePlayer();
  const sent: string[] = [];
  const toasts: string[] = [];
  const controller = new VoiceController({
    serverIdentity: "server-a",
    sessionKey: "%7",
    paneId: "%7",
    sessionId: "$1",
    store,
    getConnection: () => connection.asHostConnection(),
    recorder: new FakeRecorder(),
    recorderCoordinator: new VoiceRecorderCoordinator(),
    player,
    files: new FakeFiles(),
    appInForeground: () => true,
    tailHoldMs: 0,
    submit: async (text) => { sent.push(text); },
    registerWithHost: false,
    toast: (message) => toasts.push(message),
  });
  return { connection, store, player, sent, toasts, controller };
}

describe("voice mode inside a chat", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("registers nothing with the host and sends a transcript as a chat message", async () => {
    const { connection, controller, sent } = harness();
    // The shared voice mode replaces a terminal's controller for a pane that became a chat.
    expect(controller.forChat).toBe(true);
    controller.focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(connection.of(Operation.VOICE_SESSION)).toHaveLength(0);
    controller.beginUtterance();
    await vi.advanceTimersByTimeAsync(0);
    await controller.endUtterance();
    expect(sent).toEqual(["add the fixture and run the tests"]);
    expect(connection.of(Operation.TERMINAL_INPUT)).toHaveLength(0);
    // A reconnect does not register it either.
    controller.onConnected();
    await vi.advanceTimersByTimeAsync(0);
    expect(connection.of(Operation.VOICE_SESSION)).toHaveLength(0);
    controller.dispose();
  });

  it("a status check nobody asked for fails quietly, where one on screen says so", async () => {
    const { connection, controller, toasts } = harness();
    connection.answer(Operation.VOICE_STATUS, () => { throw new HostError("internal", "The host could not check voice."); });
    await controller.refreshStatus(false, true);
    expect(toasts).toEqual([]);
    await controller.refreshStatus(false);
    expect(toasts).toHaveLength(1);
    controller.dispose();
  });

  it("reads a finished reply aloud, and offers Retry when synthesis fails", async () => {
    const { connection, controller, store, player } = harness();
    controller.focus();
    await vi.advanceTimersByTimeAsync(0);
    await controller.speakReply("All **fourteen** link tests pass.", 1_767_225_660_000);
    expect(connection.of(Operation.VOICE_SPEAK)[0]!.voice!.text).toBe("All **fourteen** link tests pass.");
    let reply = latestReply(store.getState().sessions["%7"]);
    expect(reply).toMatchObject({ displayText: "All **fourteen** link tests pass.", at: 1_767_225_660_000 });
    expect(reply?.fileUri).toBeDefined();
    expect(player.calls).toContain("play");

    connection.answer(Operation.VOICE_SPEAK, () => { throw new HostError("voice_model_missing", "Voice isn't set up on this host."); });
    await controller.speakReply("Not pushed.", 1_767_225_720_000);
    reply = latestReply(store.getState().sessions["%7"]);
    expect(reply).toMatchObject({ displayText: "Not pushed.", audioError: expect.any(String) });
    controller.dispose();
  });
});
