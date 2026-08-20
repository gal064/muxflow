import { beforeEach, describe, expect, it, vi } from "vitest";
import { TauriAgentClient } from "./api";
import { agentGeneration } from "./generation";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const scope = { clientId: "client", hostProfileId: "local", serverIdentity: "server-a", topologyGeneration: 9, connectionEpoch: 4 };
const wireRecord = {
  agentId: "agent-1", adapter: "codex", adapterId: "codex", nativeSessionId: "native", displayName: "Review bot",
  route: { hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1", sessionNameFallback: "work", windowId: "@1", windowNameFallback: "agent", paneId: "%1", agentId: "agent-1", attentionGeneration: "4" },
  lifecycle: "blocked", authority: "hook", stateGeneration: "5", attentionGeneration: "4", attentionKind: "blocked", seenGeneration: "2",
  updatedAtUnixMillis: "100", hookAuthorityExpiresAtUnixMillis: "200", detectedManually: true,
  present: true,
};

describe("TauriAgentClient", () => {
  beforeEach(() => invokeMock.mockReset());

  it("maps the protobuf-derived JSON snapshot into the adapter-neutral record", async () => {
    invokeMock.mockResolvedValue({ snapshot: { generation: "6", acceptedGeneration: "6", agents: [wireRecord], authoritative: true, notificationWatermark: "4", connectionEpoch: "4", adapters: [{ adapter: "unspecified", id: "future", displayName: "Future", supportsLaunch: true, supportsResume: true, supportsHooks: false, supportsProcessDetection: true, supportsScreenFallback: false, hookConfigPath: "", hookEvents: [] }] } });
    const snapshot = await new TauriAgentClient().snapshot(scope);
    expect(snapshot).toMatchObject({ revision: "6", acceptedGeneration: "6", notificationWatermark: "4", authoritative: true, adapters: [{ id: "future", displayName: "Future", placements: ["window", "split"], supportsProcessDetection: true }], agents: [{ id: "agent-1", adapterId: "codex", lifecycle: "blocked", attentionGeneration: "4", attentionKind: "blocked", paneId: "%1", detectedManually: true }] });
    // `wireRecord` deliberately still carries `authority` and
    // `hookAuthorityExpiresAtUnixMillis`, and the adapter still carries
    // `supportsScreenFallback`. A host older than this desktop keeps sending
    // them; they must be ignored rather than surfaced or rejected.
    expect(snapshot.agents[0]).not.toHaveProperty("authority");
    expect(invokeMock).toHaveBeenCalledWith("agent_request", { clientId: "client", command: expect.objectContaining({ operation: "snapshot", expectedServerIdentity: "server-a", expectedTopologyGeneration: "9", connectionEpoch: "4" }) });
  });

  // These mappers used to throw. Nothing catches per record, so one value this
  // build did not recognise aborted `mapSnapshot` and the user's entire agent
  // list went blank — from a host merely newer than the desktop.
  it("degrades an unrecognised lifecycle to unknown instead of blanking the list", async () => {
    const strange = { ...wireRecord, agentId: "agent-2", route: { ...wireRecord.route, agentId: "agent-2" }, lifecycle: "deliberating", attentionKind: "pondering" };
    invokeMock.mockResolvedValue({ snapshot: { generation: "6", acceptedGeneration: "6", agents: [wireRecord, strange], authoritative: true, notificationWatermark: "4", connectionEpoch: "4", adapters: [] } });
    const snapshot = await new TauriAgentClient().snapshot(scope);
    expect(snapshot.agents).toHaveLength(2);
    expect(snapshot.agents[0].lifecycle).toBe("blocked");
    expect(snapshot.agents[1].lifecycle).toBe("unknown");
    expect(snapshot.agents[1].attentionKind).toBeUndefined();
  });

  it("passes an opaque registry adapter ID through launch and binds seen to an exact generation", async () => {
    invokeMock.mockResolvedValue({});
    const client = new TauriAgentClient();
    await client.launch(scope, { adapterId: "future-agent", placement: "split", sessionId: "$1", windowId: "@1", paneId: "%1", activeRoot: "/repo", rootToken: "root-token" });
    await client.markSeen(scope, "agent-1", agentGeneration(4));
    expect(invokeMock.mock.calls[0][1].command).toMatchObject({ operation: "launchSplit", adapterId: "future-agent", activeRoot: "/repo", rootToken: "root-token" });
    expect(invokeMock.mock.calls[1][1].command).toMatchObject({ operation: "markSeen", agentId: "agent-1", attentionGeneration: "4" });
  });

  it("sends resume placement with the complete active root context", async () => {
    invokeMock.mockResolvedValue({});
    const client = new TauriAgentClient();
    const request = { adapterId: "codex", placement: "window" as const, sessionId: "$1", windowId: "@1", paneId: "%1", activeRoot: "/repo", rootToken: "root-token" };
    await client.resume(scope, "agent-1", "native-1", request);
    expect(invokeMock.mock.calls[0][1].command).toMatchObject({ operation: "resume", agentId: "agent-1", nativeSessionId: "native-1", placement: "window", sessionId: "$1", windowId: "@1", paneId: "", activeRoot: "/repo", rootToken: "root-token" });
  });

  it("publishes the accepted rename RPC response immediately through the reducer stream", async () => {
    const renamed = { ...wireRecord, displayName: "Renamed now" };
    invokeMock.mockResolvedValue({ agent: renamed, acceptedGeneration: "7", connectionEpoch: "4" });
    const client = new TauriAgentClient();
    const listener = vi.fn();
    client.subscribe(listener);
    await client.rename(scope, "agent-1", " Renamed now ");
    expect(listener).toHaveBeenCalledWith(expect.objectContaining({
      kind: "upsert", sequence: "7", record: expect.objectContaining({ id: "agent-1", displayName: "Renamed now" }),
    }));
  });

  it("requires a host-issued confirmation token before hook install or uninstall", async () => {
    invokeMock.mockResolvedValueOnce({ hookPlan: { adapter: "claudeCode", adapterId: "claude-code", action: "review", configPath: "/config", backupPath: "/backup", managedVersion: "muxflow/v1", summary: "one managed hook", confirmationToken: "confirm", alreadyCurrent: false, ownershipMarker: "Claude adapter", proposedCommand: "/usr/bin/host hook ingest", proposedEvents: ["Stop"], trustGuidance: "Review trust", beforeHash: "old", afterHash: "new", createsConfig: false, removesConfig: true, beforePreview: "{\n  redacted\n}", afterPreview: "{\n  managed\n}", diffPreview: "--- before\n-redacted\n+++ after\n+managed", previewTruncated: false } }).mockResolvedValueOnce({});
    const client = new TauriAgentClient();
    const review = await client.reviewHooks(scope, "claude-code", "uninstall");
    expect(review).toMatchObject({ action: "uninstall", revision: "confirm", trustGuidance: "Review trust", changes: [{ path: "/config", owner: "Claude adapter", command: "/usr/bin/host hook ingest", events: ["Stop"], beforeHash: "old", afterHash: "new", removesConfig: true, diffPreview: expect.stringContaining("--- before") }] });
    await client.applyHooks(scope, review);
    expect(invokeMock.mock.calls[1][1].command).toMatchObject({ operation: "hookUninstall", confirmed: true, confirmationToken: "confirm" });
  });

  it("publishes manual-to-native retirements in the same ordered upsert", () => {
    const client = new TauriAgentClient();
    const listener = vi.fn();
    client.subscribe(listener);
    client.publishWireEvent(scope, { agent: wireRecord, generation: "18446744073709551615", connectionEpoch: "4", retiredAgentIds: ["manual-1"] });
    expect(listener).toHaveBeenCalledWith(expect.objectContaining({
      kind: "upsert", sequence: "18446744073709551615", retiredAgentIds: ["manual-1"],
    }));
  });

  it("drops AgentService payloads from a replaced bridge epoch", () => {
    const client = new TauriAgentClient();
    const listener = vi.fn();
    client.subscribe(listener);
    client.publishWireEvent(scope, { agent: wireRecord, generation: "7", connectionEpoch: "3" });
    expect(listener).not.toHaveBeenCalled();
  });

  it("retains an unmapped wire record with empty canonical route IDs", async () => {
    const unmapped = {
      ...wireRecord,
      route: { ...wireRecord.route, sessionId: "", sessionNameFallback: "", windowId: "", windowNameFallback: "", paneId: "" },
    };
    invokeMock.mockResolvedValue({ snapshot: { generation: "8", acceptedGeneration: "8", agents: [unmapped], authoritative: true, notificationWatermark: "8", connectionEpoch: "4", adapters: [] } });
    const snapshot = await new TauriAgentClient().snapshot(scope);
    expect(snapshot.agents[0]).toMatchObject({ paneId: "", sessionId: "", windowId: "" });
  });
});
