import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { EventKind, HostEventSchema, Operation, VoiceEventSchema, VoiceProvisionProgressSchema, VoiceSpeechSchema, VoiceStatusSchema } from "../../protocol/gen/envelope_pb";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder, transcriptResponse } from "./testing";
import { VoiceRegistry } from "./voiceRegistry";
import { createVoiceStore, latestReply } from "./voiceStore";

const settle = async () => {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
};

function harness() {
  const store = createVoiceStore();
  const connection = new FakeConnection();
  const logs: string[] = [];
  let foreground = true;
  const registry = new VoiceRegistry(store, (line) => logs.push(line));
  const deps = { serverIdentity: "server-a", tailHoldMs: 0, getConnection: () => connection.asHostConnection(), recorder: new FakeRecorder(), player: new FakePlayer(), files: new FakeFiles(), appInForeground: () => foreground };
  return { store, connection, registry, deps, logs, setForeground: (active: boolean) => { foreground = active; } };
}

describe("VoiceRegistry", () => {
  it("routes VOICE_REPLY by pane, drops it for an unknown pane, and applies VOICE_PROVISION", () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    expect(h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps })).toBe(a);
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_REPLY, voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { serverIdentity: "server-a", paneId: "%1", displayMarkdown: "**hi**", speechText: "hi", audio: new Uint8Array([1]) }) }) }));
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_REPLY, voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { serverIdentity: "server-a", paneId: "%9", displayMarkdown: "no", speechText: "no", audio: new Uint8Array([1]) }) }) }));
    expect(h.store.getState().sessions["%1"]?.messages.map((m) => m.displayText)).toEqual(["**hi**"]);
    expect(h.store.getState().sessions["%9"]).toBeUndefined();
    // A failed synthesis carries the error in the event's status detail.
    h.registry.onVoiceEvent(create(HostEventSchema, {
      kind: EventKind.VOICE_REPLY,
      voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { serverIdentity: "server-a", paneId: "%1", displayMarkdown: "_silent_", speechText: "silent" }), status: create(VoiceStatusSchema, { detail: "tts down" }) }),
    }));
    expect(h.store.getState().sessions["%1"]?.messages.at(-1)).toMatchObject({ displayText: "_silent_", speechText: "silent", audioError: "tts down" });
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_PROVISION, voice: create(VoiceEventSchema, { provision: create(VoiceProvisionProgressSchema, { phase: "verifying", totalBytes: 4n, transferredBytes: 4n }) }) }));
    expect(h.store.getState().hostStatus).toMatchObject({ readiness: "provisioning", provision: { phase: "verifying" } });
  });

  it("onConnected re-registers live sessions; end clears on the host; disposeAll is local only", async () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await Promise.resolve();
    h.registry.onConnected("server-a");
    await Promise.resolve();
    // Only the focused-at-least-once session is registered; b never opened a screen.
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.paneId)).toEqual(["%1", "%1"]);
    // b opens its screen too, so both are registered; End on a clears the whole
    // connection's registrations, so b registers again right after.
    h.registry.get("%2")!.focus();
    await Promise.resolve();
    await h.registry.end("%1");
    await Promise.resolve();
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.paneId)).toEqual(["%1", "%1", "%2", "", "%2"]);
    expect(h.registry.get("%1")).toBeUndefined();
    h.registry.disposeAll();
    expect(h.registry.get("%2")).toBeUndefined();
    expect(h.store.getState().sessions).toEqual({});
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(5);
  });

  it("does not carry a pane conversation across a tmux server replacement", () => {
    const h = harness();
    h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    expect(h.store.getState().sessions["%1"]).toBeDefined();

    h.registry.onServerChanged("server-b");
    expect(h.registry.get("%1")).toBeUndefined();
    expect(h.store.getState().sessions).toEqual({});

    h.registry.open({ ...h.deps, serverIdentity: "server-b", paneId: "%1", sessionId: "$2" });
    h.registry.onVoiceEvent(create(HostEventSchema, {
      kind: EventKind.VOICE_REPLY,
      voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, {
        serverIdentity: "server-a", paneId: "%1", displayMarkdown: "stale", speechText: "stale",
      }) }),
    }));
    expect(h.store.getState().sessions["%1"]?.messages).toEqual([]);
  });

  it("keeps B's reply manual-only when it arrives while A owns the listening microphone", async () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    a.beginUtterance();
    await settle();

    h.registry.onVoiceEvent(create(HostEventSchema, {
      kind: EventKind.VOICE_REPLY,
      voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { serverIdentity: "server-a", paneId: "%2", displayMarkdown: "For B", speechText: "For B", audio: new Uint8Array([1]) }) }),
    }));
    const message = latestReply(h.store.getState().sessions["%2"])!;
    expect(message.played).toBe(false);

    await a.cancelUtterance();
    a.blur();
    b.focus();
    await settle();
    expect(message.played).toBe(false);
    expect(h.deps.player.calls).not.toContain("play");
    h.registry.disposeAll();
  });

  it("hands the recorder to B after A's normal release has safely consumed its recording", async () => {
    const h = harness();
    const events: string[] = [];
    const nativePrepare = h.deps.recorder.prepare.bind(h.deps.recorder);
    const nativeRecord = h.deps.recorder.record.bind(h.deps.recorder);
    const nativeRead = h.deps.files.read.bind(h.deps.files);
    let finishRead!: () => void;
    h.deps.recorder.prepare = async () => { events.push("prepare"); await nativePrepare(); };
    h.deps.recorder.record = () => { events.push("record"); nativeRecord(); };
    h.deps.files.read = async (uri) => {
      events.push("read.begin");
      await new Promise<void>((resolve) => { finishRead = resolve; });
      events.push("read.end");
      return nativeRead(uri);
    };
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    a.beginUtterance();
    await settle();
    events.length = 0;

    const ending = a.endUtterance();
    await settle();
    a.blur();
    b.focus();
    b.beginUtterance();
    await settle();
    expect(events).toEqual(["read.begin"]);

    finishRead();
    await ending;
    await settle();
    expect(events).toEqual(["read.begin", "read.end", "prepare", "record"]);
    expect(h.deps.recorder.recording).toBe(true);
    expect(h.store.getState().sessions["%2"]?.phase).toBe("recording");
    h.registry.disposeAll();
    await settle();
  });

  it("keeps B's recorder claim pending when its focus arrives before A's blur", async () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    expect(h.deps.recorder.prepared).toBe(1);

    b.focus();
    await settle();
    expect(h.deps.recorder.prepared).toBe(1);
    a.blur();
    await settle();
    expect(h.deps.recorder.released).toBe(1);
    expect(h.deps.recorder.prepared).toBe(1);

    b.beginUtterance();
    await settle();
    expect(h.deps.recorder.recording).toBe(true);
    expect(h.store.getState().sessions["%2"]?.phase).toBe("recording");
    h.registry.disposeAll();
    await settle();
  });

  it("releases an abandoned recording before the next session transcribes", async () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    a.beginUtterance();
    await settle();

    // Navigating away can unmount the mic before React Native sends press-out.
    // The old session must not retain the one native recorder.
    a.blur();
    b.focus();
    await settle();
    expect(h.store.getState().sessions["%1"]?.phase).toBe("idle");

    h.deps.recorder.nextUri = "file:///cache/rec-2.m4a";
    h.deps.files.files.set("file:///cache/rec-2.m4a", new TextEncoder().encode("second-aac"));
    b.beginUtterance();
    await settle();
    expect(h.deps.recorder.recording).toBe(true);
    await b.endUtterance();

    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(1);
    expect(h.store.getState().sessions["%2"]?.phase).toBe("idle");
    h.registry.disposeAll();
    await settle();
  });

  it("does not grant a canceled recorder claim after the app enters the background", async () => {
    const h = harness();
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    b.focus();
    b.beginUtterance();
    await settle();
    expect(h.store.getState().sessions["%2"]?.phase).toBe("recording");
    expect(h.deps.recorder.recording).toBe(false); // B is waiting behind A's prepared recorder.

    h.setForeground(false);
    b.onAppInactive();
    a.blur();
    await settle();

    expect(h.store.getState().sessions["%2"]?.phase).toBe("idle");
    expect(h.deps.recorder.prepared).toBe(0);
    expect(h.deps.recorder.recording).toBe(false);

    h.setForeground(true);
    b.onAppActive();
    await settle();
    expect(h.deps.recorder.prepared).toBe(1);
    h.registry.disposeAll();
    await settle();
  });

  it("lets B record while A's captured utterance is still transcribing", async () => {
    const h = harness();
    let transcriptions = 0;
    let answerA: ((response: ReturnType<typeof transcriptResponse>) => void) | undefined;
    h.connection.answer(Operation.VOICE_TRANSCRIBE, () => {
      transcriptions += 1;
      if (transcriptions === 1) return new Promise((resolve) => { answerA = resolve; });
      return transcriptResponse("from B");
    });
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    a.beginUtterance();
    await settle();
    const endingA = a.endUtterance();
    await settle();
    expect(h.store.getState().sessions["%1"]?.phase).toBe("transcribing");

    a.blur();
    b.focus();
    await settle();
    h.deps.recorder.nextUri = "file:///cache/rec-2.m4a";
    h.deps.files.files.set("file:///cache/rec-2.m4a", new TextEncoder().encode("second-aac"));
    b.beginUtterance();
    await settle();
    expect(h.deps.recorder.recording).toBe(true);
    await b.endUtterance();

    expect(h.connection.of(Operation.VOICE_TRANSCRIBE)).toHaveLength(2);
    expect(h.store.getState().sessions["%2"]?.phase).toBe("idle");
    answerA!(transcriptResponse("from A"));
    await endingA;
    expect(h.store.getState().sessions["%1"]?.phase).toBe("idle");
    h.registry.disposeAll();
    await settle();
  });

  it("does not let A's delayed cancellation release the recorder after B claims it", async () => {
    const h = harness();
    const events: string[] = [];
    const nativePrepare = h.deps.recorder.prepare.bind(h.deps.recorder);
    const nativeRecord = h.deps.recorder.record.bind(h.deps.recorder);
    const nativeStop = h.deps.recorder.stop.bind(h.deps.recorder);
    const nativeRelease = h.deps.recorder.release.bind(h.deps.recorder);
    let finishStop!: () => void;
    h.deps.recorder.prepare = async () => { events.push("prepare"); await nativePrepare(); };
    h.deps.recorder.record = () => { events.push("record"); nativeRecord(); };
    h.deps.recorder.stop = async () => {
      events.push("stop.begin");
      await new Promise<void>((resolve) => { finishStop = resolve; });
      events.push("stop.end");
      return nativeStop();
    };
    h.deps.recorder.release = () => { events.push("release"); nativeRelease(); };
    const a = h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const b = h.registry.open({ paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await settle();
    a.beginUtterance();
    await settle();
    events.length = 0;

    const canceling = a.cancelUtterance();
    await settle();
    a.blur();
    b.focus();
    b.beginUtterance();
    await settle();
    expect(events).toEqual(["stop.begin"]);
    expect(h.deps.recorder.released).toBe(0);

    finishStop();
    await canceling;
    await settle();
    expect(events).toEqual(["stop.begin", "stop.end", "release", "prepare", "record"]);
    expect(h.deps.recorder.recording).toBe(true);
    expect(h.deps.recorder.released).toBe(1);
    expect(h.store.getState().sessions["%2"]?.phase).toBe("recording");
    h.deps.recorder.stop = nativeStop;
    h.registry.disposeAll();
    await settle();
  });
});

describe("VoiceRegistry host-global state (review round 2)", () => {
  it("open() keeps one controller per pane and refreshes its session route", () => {
    const h = harness();
    h.registry.open({ paneId: "%1", sessionId: "$1", ...h.deps });
    const same = h.registry.open({ paneId: "%1", sessionId: "$7", ...h.deps });
    expect(same.target).toEqual({ paneId: "%1", sessionId: "$7" });
    expect(h.store.getState().sessions["%1"]?.sessionId).toBe("$7");
    h.store.getState().setReadiness("ready", "");
    h.registry.disposeAll();
    expect(h.store.getState().hostStatus.readiness).toBe("unknown");
  });
});
