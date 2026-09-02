import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { EventKind, HostEventSchema, Operation, VoiceEventSchema, VoiceProvisionProgressSchema, VoiceSpeechSchema, VoiceStatusSchema } from "../../protocol/gen/envelope_pb";
import { FakeConnection, FakeFiles, FakePlayer, FakeRecorder } from "./testing";
import { VoiceRegistry } from "./voiceRegistry";
import { createVoiceStore } from "./voiceStore";

function harness() {
  const store = createVoiceStore();
  const connection = new FakeConnection();
  const registry = new VoiceRegistry(store);
  const deps = { getConnection: () => connection.asHostConnection(), recorder: new FakeRecorder(), player: new FakePlayer(), files: new FakeFiles(), appInForeground: () => true };
  return { store, connection, registry, deps };
}

describe("VoiceRegistry", () => {
  it("routes VOICE_REPLY to the named session, drops it for an unknown agent, and applies VOICE_PROVISION to the host status", () => {
    const h = harness();
    const a = h.registry.open({ agentId: "a", paneId: "%1", sessionId: "$1", ...h.deps });
    expect(h.registry.open({ agentId: "a", paneId: "%1", sessionId: "$1", ...h.deps })).toBe(a);
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_REPLY, voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { agentId: "a", text: "hi", audio: new Uint8Array([1]) }) }) }));
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_REPLY, voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { agentId: "ghost", text: "no", audio: new Uint8Array([1]) }) }) }));
    expect(h.store.getState().sessions["a"]?.messages.map((m) => m.text)).toEqual(["hi"]);
    expect(h.store.getState().sessions["ghost"]).toBeUndefined();
    // A failed synthesis carries the error in the event's status detail.
    h.registry.onVoiceEvent(create(HostEventSchema, {
      kind: EventKind.VOICE_REPLY,
      voice: create(VoiceEventSchema, { reply: create(VoiceSpeechSchema, { agentId: "a", text: "silent" }), status: create(VoiceStatusSchema, { detail: "tts down" }) }),
    }));
    expect(h.store.getState().sessions["a"]?.messages.at(-1)).toMatchObject({ text: "silent", audioError: "tts down" });
    h.registry.onVoiceEvent(create(HostEventSchema, { kind: EventKind.VOICE_PROVISION, voice: create(VoiceEventSchema, { provision: create(VoiceProvisionProgressSchema, { phase: "extracting", totalBytes: 4n, transferredBytes: 4n }) }) }));
    expect(h.store.getState().hostStatus).toMatchObject({ readiness: "provisioning", provision: { phase: "extracting" } });
  });

  it("onConnected re-registers live sessions; end clears on the host; disposeAll is local only", async () => {
    const h = harness();
    const a = h.registry.open({ agentId: "a", paneId: "%1", sessionId: "$1", ...h.deps });
    h.registry.open({ agentId: "b", paneId: "%2", sessionId: "$1", ...h.deps });
    a.focus();
    await Promise.resolve();
    h.registry.onConnected();
    await Promise.resolve();
    // Only the focused-at-least-once session is registered; b never opened a screen.
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["a", "a"]);
    // b opens its screen too, so both are registered; End on a clears the whole
    // connection's registrations, so b registers again right after.
    h.registry.get("b")!.focus();
    await Promise.resolve();
    await h.registry.end("a");
    await Promise.resolve();
    expect(h.connection.of(Operation.VOICE_SESSION).map((r) => r.voice?.agentId)).toEqual(["a", "a", "b", "", "b"]);
    expect(h.registry.get("a")).toBeUndefined();
    h.registry.disposeAll();
    expect(h.registry.get("b")).toBeUndefined();
    expect(h.store.getState().sessions).toEqual({});
    expect(h.connection.of(Operation.VOICE_SESSION)).toHaveLength(5);
  });
});

describe("VoiceRegistry host-global state (review round 2)", () => {
  it("open() re-points an existing session at the agent's current pane; disposeAll forgets the host status", () => {
    const h = harness();
    h.registry.open({ agentId: "a", paneId: "%1", sessionId: "$1", ...h.deps });
    const same = h.registry.open({ agentId: "a", paneId: "%7", sessionId: "$1", ...h.deps });
    expect(same.target).toEqual({ paneId: "%7", sessionId: "$1" });
    expect(h.store.getState().sessions["a"]?.paneId).toBe("%7");
    h.store.getState().setReadiness("ready", "");
    h.registry.disposeAll();
    expect(h.store.getState().hostStatus.readiness).toBe("unknown");
  });
});
