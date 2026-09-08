import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { VoiceProvisionProgressSchema, VoiceReadiness, VoiceStatusSchema } from "../../protocol/gen/envelope_pb";
import type { Agent } from "../../store/sessionStore";
import { awaitingReply, createVoiceStore, hostStatusFromProto, latestReply, provisionFromProto, type VoiceMessage } from "./voiceStore";

function message(id: string, kind: VoiceMessage["kind"], fileUri?: string): VoiceMessage {
  return { id, kind, displayText: id, speechText: id, at: 1, truncated: false, fileUri, audioError: undefined, played: kind === "you" };
}

function promotedAgent(): Agent {
  return {
    id: "native",
    adapterId: "codex",
    nativeSessionId: "native-session",
    displayName: "Codex",
    lifecycle: "working",
    attentionKind: "",
    stateGeneration: 2n,
    attentionGeneration: 0n,
    seenGeneration: 0n,
    updatedAtMs: 2,
    lifecycleChangedAtMs: 2,
    attentionSeenAtMs: 0,
    present: true,
    route: { sessionId: "$2", sessionNameFallback: "", windowId: "@2", windowNameFallback: "", paneId: "%7", paneIndexFallback: 0 },
  };
}

describe("voiceStore", () => {
  it("maps VoiceStatus, including the resumed provision progress", () => {
    const status = hostStatusFromProto(create(VoiceStatusSchema, {
      readiness: VoiceReadiness.PROVISIONING,
      detail: "d",
      uvPath: "/u/uv",
      modelDownloadBytes: 10n,
      sidecarRunning: true,
      provision: create(VoiceProvisionProgressSchema, { phase: "downloading", transferredBytes: 5n, totalBytes: 10n }),
    }));
    expect(status).toEqual({ readiness: "provisioning", detail: "d", uvPath: "/u/uv", modelDownloadBytes: 10, sidecarRunning: true, provision: { phase: "downloading", transferredBytes: 5, totalBytes: 10, error: "" } });
    expect(hostStatusFromProto(create(VoiceStatusSchema, { readiness: VoiceReadiness.UV_MISSING })).readiness).toBe("uvMissing");
    expect(hostStatusFromProto(create(VoiceStatusSchema, { readiness: VoiceReadiness.MODEL_MISSING })).readiness).toBe("modelMissing");
    expect(hostStatusFromProto(create(VoiceStatusSchema, {})).readiness).toBe("unknown");
  });

  it("provision progress moves readiness: in flight → provisioning, ready → ready, failed → modelMissing with the error kept", () => {
    const store = createVoiceStore();
    store.getState().applyProvisionProgress(provisionFromProto(create(VoiceProvisionProgressSchema, { phase: "downloading", transferredBytes: 1n, totalBytes: 2n })));
    expect(store.getState().hostStatus.readiness).toBe("provisioning");
    store.getState().applyProvisionProgress({ phase: "failed", transferredBytes: 1, totalBytes: 2, error: "boom" });
    expect(store.getState().hostStatus.readiness).toBe("modelMissing");
    expect(store.getState().hostStatus.provision?.error).toBe("boom");
    store.getState().applyProvisionProgress({ phase: "ready", transferredBytes: 2, totalBytes: 2, error: "" });
    expect(store.getState().hostStatus.readiness).toBe("ready");
    expect(store.getState().hostStatus.provision).toBeUndefined();
  });

  it("ensureSession keeps history and re-points the pane; removeSession forgets and drops its playback", () => {
    const store = createVoiceStore();
    store.getState().ensureSession("a", "%1", "$1", 10);
    store.getState().appendMessage("a", message("m1", "you"));
    store.getState().ensureSession("a", "%2", "$1", 20);
    expect(store.getState().sessions["a"]).toMatchObject({ paneId: "%2", startedAt: 10 });
    expect(store.getState().sessions["a"]?.messages).toHaveLength(1);
    store.getState().appendReply("a", message("r1", "agent", "file:///r1"));
    store.getState().setPlayback({ messageId: "r1", state: "playing", positionMs: 0, durationMs: 0 });
    store.getState().removeSession("a");
    expect(store.getState().sessions["a"]).toBeUndefined();
    expect(store.getState().playback).toBeUndefined();
  });

  it("appendReply strips the previous reply's file and reports it; latestReply finds the newest agent turn", () => {
    const store = createVoiceStore();
    store.getState().ensureSession("a", "%1", "$1", 0);
    expect(store.getState().appendReply("a", message("r1", "agent", "file:///r1"))).toBeUndefined();
    store.getState().appendMessage("a", message("m2", "you"));
    expect(store.getState().appendReply("a", message("r2", "agent", "file:///r2"))).toBe("file:///r1");
    const messages = store.getState().sessions["a"]!.messages;
    expect(messages.map((m) => [m.id, m.fileUri])).toEqual([["r1", undefined], ["m2", undefined], ["r2", "file:///r2"]]);
    expect(latestReply(store.getState().sessions["a"])?.id).toBe("r2");
    expect(latestReply(undefined)).toBeUndefined();
  });

  it("promotes the remote identity without moving or replacing local session state", () => {
    const store = createVoiceStore();
    store.getState().ensureSession("manual", "%7", "$1", 10);
    store.getState().appendMessage("manual", message("m1", "you", "file:///recording"));
    store.getState().setPhase("manual", "recordingLocked");
    store.getState().setPlayback({ messageId: "m1", state: "paused", positionMs: 120, durationMs: 500 });
    const before = store.getState().sessions.manual!;
    const messages = before.messages;
    const playback = store.getState().playback;

    store.getState().promoteSession("manual", promotedAgent());

    const after = store.getState().sessions.manual!;
    expect(store.getState().sessions.native).toBeUndefined();
    expect(after).toMatchObject({ agentId: "native", paneId: "%7", sessionId: "$2", startedAt: 10, phase: "recordingLocked" });
    expect(after.messages).toBe(messages);
    expect(store.getState().playback).toBe(playback);
  });

  it("markPlayed and setMessageAudio touch only their message and are no-ops for unknown ids", () => {
    const store = createVoiceStore();
    store.getState().ensureSession("a", "%1", "$1", 0);
    store.getState().appendReply("a", { ...message("r1", "agent"), audioError: "x" });
    const before = store.getState().sessions["a"];
    store.getState().markPlayed("a", "nope");
    store.getState().markPlayed("b", "r1");
    expect(store.getState().sessions["a"]).toBe(before);
    store.getState().setMessageAudio("a", "r1", "file:///r1");
    store.getState().markPlayed("a", "r1");
    expect(store.getState().sessions["a"]!.messages[0]).toMatchObject({ fileUri: "file:///r1", audioError: undefined, played: true });
    store.getState().setPhase("a", "recording");
    expect(store.getState().sessions["a"]!.phase).toBe("recording");
    store.getState().setPhase("zzz", "recording");
  });
});

describe("awaitingReply", () => {
  it("is true only while the last turn is the user's", () => {
    const store = createVoiceStore();
    expect(awaitingReply(undefined)).toBe(false);
    store.getState().ensureSession("a", "%1", "$1", 0);
    expect(awaitingReply(store.getState().sessions["a"])).toBe(false);
    store.getState().appendMessage("a", message("m1", "you"));
    expect(awaitingReply(store.getState().sessions["a"])).toBe(true);
    store.getState().appendReply("a", message("r1", "agent", "file:///r1"));
    expect(awaitingReply(store.getState().sessions["a"])).toBe(false);
    store.getState().appendMessage("a", message("m2", "you"));
    expect(awaitingReply(store.getState().sessions["a"])).toBe(true);
  });
});
