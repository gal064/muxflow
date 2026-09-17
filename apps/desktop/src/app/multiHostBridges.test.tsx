// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ResumeTrigger } from "../features/shell/useDesktopResumeRecovery";
import { hostProfileId } from "../features/shell/types";
import type { TerminalEvent } from "../features/terminal/api";
import { terminalCacheKey, terminalStateCache } from "../features/terminal/TerminalStateCache";
import type { ConnectionSpec, TmuxSnapshot } from "./types";
import { useAppConnectionController } from "./useAppConnectionController";

const invokeMock = vi.hoisted(() => vi.fn());
const startTerminalMock = vi.hoisted(() => vi.fn());
const stopTerminalMock = vi.hoisted(() => vi.fn(async (_clientId: string) => undefined));
const prewarmTerminalBulkMock = vi.hoisted(() => vi.fn(async (_clientId: string) => undefined));
const requestTerminalSeedMock = vi.hoisted(() => vi.fn(async (_clientId: string, _paneId: string) => undefined));
const clipboardMock = vi.hoisted(() => vi.fn(async (_enabled: boolean, _text: string) => undefined));
const resume = vi.hoisted(() => ({
  trigger: undefined as ((trigger: ResumeTrigger) => void) | undefined,
  outcome: "alive" as "alive" | "dead",
}));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
// The bridges are not under test; how many there are, and what each is asked
// to attach, is.
vi.mock("../features/terminal/api", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/terminal/api")>(),
  startTerminal: startTerminalMock,
  stopTerminal: stopTerminalMock,
  prewarmTerminalBulk: prewarmTerminalBulkMock,
  requestTerminalSeed: requestTerminalSeedMock,
}));
vi.mock("../features/terminal/terminalTransferApi", () => ({ writeTerminalApplicationClipboard: clipboardMock }));
// The resume detectors are not under test either; what a dead link costs is.
vi.mock("../features/shell/useDesktopResumeRecovery", () => ({
  useDesktopResumeRecovery: (onResume: (trigger: ResumeTrigger) => void) => { resume.trigger = onResume; },
  probeResumedLink: async () => resume.outcome,
}));

interface Bridge {
  clientId: string;
  connection: ConnectionSpec;
  attach: boolean;
  publish(event: TerminalEvent): void;
}

const connected: TerminalEvent = { kind: "connectionState", state: "connected", sequence: 0 };
const world = (serverIdentity: string, sessions: Array<[id: string, name: string]>): TerminalEvent => {
  const snapshot: TmuxSnapshot = {
    sessions: sessions.map(([id, name]) => ({ id, name, windowCount: 1, attachedClients: 0 })),
    windows: [],
    panes: [],
  };
  return { kind: "snapshot", snapshot, generation: 1, serverIdentity, authoritative: true, sequence: 0 };
};
const calls = (command: string) => invokeMock.mock.calls.filter(([name]) => name === command).map(([, request]) => request);

interface HarnessOptions {
  /** Bridges whose `startTerminal` waits for `release()` before resolving. */
  holdStart?(connection: ConnectionSpec): boolean;
  onConnectionStateChanged?: Parameters<typeof useAppConnectionController>[0]["onConnectionStateChanged"];
  onHandshakeFailure?(connection: ConnectionSpec): void;
}

/** The controller alone, on a Local host shown beside a saved SSH host. */
async function twoShownHosts(options: HarnessOptions = {}) {
  const bridges: Bridge[] = [];
  const held: Array<() => void> = [];
  startTerminalMock.mockImplementation(async (
    _sessionId: string, _paneIds: string[], connection: ConnectionSpec, attach: boolean, onEvent: (event: TerminalEvent) => void,
  ) => {
    const clientId = `client-${bridges.length + 1}`;
    bridges.push({ clientId, connection, attach, publish: onEvent });
    if (options.holdStart?.(connection)) await new Promise<void>((resolve) => { held.push(resolve); });
    return clientId;
  });
  invokeMock.mockImplementation((command: string) => {
    if (command === "list_host_profiles") {
      return Promise.resolve({
        schemaVersion: 1,
        lastProfileId: "local",
        profiles: [
          { id: "local", label: "Local", connection: { mode: "local" }, shown: true },
          { id: "remote-a", label: "qa", connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" }, shown: true },
        ],
      });
    }
    return Promise.resolve(undefined);
  });
  const retireConnection = vi.fn();
  const agentClient = { publishWireEvent: vi.fn(), publishWireSnapshot: vi.fn() };
  const fileClient = { publishWireEvent: vi.fn(), retireConnection };
  const gitClient = { publishWireEvent: vi.fn() };
  const statuses: string[] = [];
  // Stable, like the shell's own `useState` setter: the controller keys its
  // profile load on it, and a fresh function per render re-runs that forever.
  const setStatus = (status: string) => { statuses.push(status); };
  let controller!: ReturnType<typeof useAppConnectionController>;
  function Harness() {
    controller = useAppConnectionController({
      agentClient: agentClient as never, fileClient: fileClient as never, gitClient: gitClient as never, setStatus,
      onConnectionStateChanged: options.onConnectionStateChanged,
      onHandshakeFailure: options.onHandshakeFailure,
    });
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  const bridgeFor = (profileId: string) => {
    const bridge = [...bridges].reverse().find((candidate) => hostProfileId(candidate.connection) === profileId);
    if (!bridge) throw new Error(`no bridge for ${profileId}`);
    return bridge;
  };
  return {
    agentClient,
    bridges,
    bridgeFor,
    controller: () => controller,
    fileClient,
    gitClient,
    release: () => { for (const resolve of held.splice(0)) resolve(); },
    renderer,
    retireConnection,
    statuses,
    unmount: () => act(async () => renderer.unmount()),
  };
}

describe("one bridge per shown host", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    startTerminalMock.mockReset();
    stopTerminalMock.mockClear();
    prewarmTerminalBulkMock.mockClear();
    resume.trigger = undefined;
    resume.outcome = "alive";
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("starts every shown host's bridge at once, attaching a terminal only on the active one", async () => {
    const { bridges, controller, unmount } = await twoShownHosts();
    expect(bridges.map((bridge) => [hostProfileId(bridge.connection), bridge.attach])).toEqual([
      ["local", true],
      ["remote-a", false],
    ]);
    expect(controller().links.map((link) => link.profileId)).toEqual(["local", "remote-a"]);
    expect(controller().activeProfileId).toBe("local");
    expect(controller().clientId).toBe("client-1");
    expect(controller().linkFor("remote-a")?.clientId).toBe("client-2");
    // Two hubs, one per host; the facade's is the active host's.
    expect(controller().hubFor("local")).toBe(controller().hub);
    expect(controller().hubFor("remote-a")).not.toBe(controller().hub);
    await unmount();
  });

  it("prewarms bulk transport only for the active host and follows activation", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    await act(async () => {
      bridgeFor("local").publish({ kind: "generationEpoch", epoch: 11, sequence: 0 });
      bridgeFor("local").publish(connected);
      bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      bridgeFor("remote-a").publish({ kind: "generationEpoch", epoch: 21, sequence: 0 });
      bridgeFor("remote-a").publish(connected);
      bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"]]));
    });
    expect(prewarmTerminalBulkMock.mock.calls).toEqual([["client-1"]]);

    // A background reconnect changes the peer's epoch but does not consume a
    // bulk-pool slot while Local remains active.
    await act(async () => {
      bridgeFor("remote-a").publish({ kind: "connectionState", state: "reconnecting", sequence: 0 });
      bridgeFor("remote-a").publish({ kind: "generationEpoch", epoch: 22, sequence: 0 });
      bridgeFor("remote-a").publish(connected);
    });
    expect(prewarmTerminalBulkMock.mock.calls).toEqual([["client-1"]]);

    await act(async () => { controller().activateHost("remote-a"); });
    expect(prewarmTerminalBulkMock.mock.calls).toEqual([["client-1"], ["client-2"]]);
    await unmount();
  });

  it("activates a peer by selecting its remembered session on its own client, restarting nothing", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    await act(async () => {
      bridgeFor("local").publish(connected);
      bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      bridgeFor("remote-a").publish(connected);
      bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    // The peer keeps its own selection while it waits offscreen.
    expect(controller().linkFor("remote-a")?.activeSessionId).toBe("$1");
    expect(controller().activeSessionId).toBe("$0");

    await act(async () => { controller().activateHost("remote-a"); });
    expect(calls("select_terminal_session")).toEqual([{ clientId: "client-2", sessionId: "$1" }]);
    expect(calls("set_last_profile_id")).toEqual([{ profileId: "remote-a" }]);
    expect(startTerminalMock).toHaveBeenCalledTimes(2);
    expect(stopTerminalMock).not.toHaveBeenCalled();
    // The facade now speaks for the peer, in full.
    expect(controller().activeProfileId).toBe("remote-a");
    expect(controller().currentHostProfileId).toBe("remote-a");
    expect(controller().connection).toEqual({ mode: "ssh", profileId: "remote-a", target: "qa-host" });
    expect(controller().clientId).toBe("client-2");
    expect(controller().clientIdRef.current).toBe("client-2");
    expect(controller().clientHostProfileId).toBe("remote-a");
    expect(controller().activeSessionId).toBe("$1");
    expect(controller().snapshot.sessions.map((session) => session.id)).toEqual(["$1", "$2"]);
    expect(controller().hostState.serverIdentity).toBe("srv-qa");
    expect(controller().hub).toBe(controller().hubFor("remote-a"));
    expect(controller().currentHostScope).toMatchObject({ hostProfileId: "remote-a", serverIdentity: "srv-qa" });

    // Back again lands on the session Local was left on.
    await act(async () => { controller().activateHost("local"); });
    expect(calls("select_terminal_session")).toEqual([
      { clientId: "client-2", sessionId: "$1" },
      { clientId: "client-1", sessionId: "$0" },
    ]);
    expect(startTerminalMock).toHaveBeenCalledTimes(2);
    expect(controller().snapshot.sessions.map((session) => session.id)).toEqual(["$0"]);
    await unmount();
  });

  it("stops a host's bridge and retires its file watches when it stops being shown", async () => {
    const { controller, retireConnection, unmount } = await twoShownHosts();
    await act(async () => {
      controller().setProfiles((current) => current.map((profile) =>
        profile.id === "remote-a" ? { ...profile, shown: false } : profile));
    });
    expect(stopTerminalMock.mock.calls).toEqual([["client-2"]]);
    expect(retireConnection.mock.calls).toEqual([["client-2"]]);
    expect(startTerminalMock).toHaveBeenCalledTimes(2);
    expect(controller().links.map((link) => link.profileId)).toEqual(["local"]);
    expect(controller().linkFor("remote-a")).toBeUndefined();
    expect(controller().clientId).toBe("client-1");
    await unmount();
  });

  it("rebuilds every link when the active link does not answer a resume, and none when it does", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    await act(async () => {
      bridgeFor("local").publish(connected);
      bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
    });
    const epochsBefore = controller().links.map((link) => link.connectionEpoch);

    resume.outcome = "alive";
    await act(async () => { resume.trigger?.("native"); });
    expect(stopTerminalMock).not.toHaveBeenCalled();
    expect(controller().links.map((link) => link.connectionEpoch)).toEqual(epochsBefore);

    resume.outcome = "dead";
    await act(async () => { resume.trigger?.("native"); });
    expect(stopTerminalMock.mock.calls.map(([clientId]) => clientId).sort()).toEqual(["client-1", "client-2"]);
    expect(startTerminalMock).toHaveBeenCalledTimes(4);
    const epochsAfter = controller().links.map((link) => link.connectionEpoch);
    expect(epochsAfter.every((epoch, index) => epoch > epochsBefore[index])).toBe(true);
    expect(controller().links.map((link) => link.detail)).toEqual([
      "System resumed; reconnecting for an authoritative state refresh.",
      "System resumed; reconnecting for an authoritative state refresh.",
    ]);
    // The rebuilt active bridge attaches, the rebuilt peer still does not.
    expect(bridgeFor("local").attach).toBe(true);
    expect(bridgeFor("remote-a").attach).toBe(false);
    await unmount();
  });

  it("keeps the active facade untouched by a peer's events", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    await act(async () => {
      bridgeFor("local").publish(connected);
      bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
    });
    const before = controller();
    await act(async () => {
      bridgeFor("remote-a").publish(connected);
      bridgeFor("remote-a").publish(world("srv-qa", [["$1", "work"]]));
      bridgeFor("remote-a").publish({ kind: "error", message: "qa-host: Connection refused", sequence: 0 });
    });
    const after = controller();
    expect(after.snapshot).toBe(before.snapshot);
    expect(after.hostState).toBe(before.hostState);
    expect(after.links[0]).toBe(before.links[0]);
    expect(after.connectionDetail).toBe("");
    expect(after.activeSessionId).toBe("$0");
    expect(after.linkFor("remote-a")).toMatchObject({
      activeSessionId: "$1",
      detail: "qa-host: Connection refused",
    });
    expect(after.linkFor("remote-a")?.hostState.sessions.$1?.name).toBe("work");
    await unmount();
  });

  it("keeps a peer's failures, clipboard, files and git off the screen", async () => {
    const { agentClient, bridgeFor, fileClient, gitClient, statuses, unmount } = await twoShownHosts();
    await act(async () => {
      bridgeFor("local").publish(connected);
      bridgeFor("remote-a").publish(connected);
      statuses.length = 0;
      bridgeFor("remote-a").publish({ kind: "error", message: "qa-host: Connection refused", sequence: 0 });
      bridgeFor("remote-a").publish({ kind: "clipboardWrite", text: "from qa", sequence: 0 });
      bridgeFor("remote-a").publish({ kind: "fileService", scope: "qa", event: { operationId: "op-qa" } as never, sequence: 0 });
      bridgeFor("remote-a").publish({ kind: "gitService", scope: "qa", event: { rootToken: "root-qa" } as never, sequence: 0 });
      bridgeFor("local").publish({ kind: "clipboardWrite", text: "from local", sequence: 0 });
      bridgeFor("local").publish({ kind: "fileService", scope: "local", event: { operationId: "op-local" } as never, sequence: 0 });
      bridgeFor("local").publish({ kind: "gitService", scope: "local", event: { rootToken: "root-local" } as never, sequence: 0 });
    });
    expect(statuses).toEqual([]);
    expect(clipboardMock.mock.calls.map(([, text]) => text)).toEqual(["from local"]);
    expect(fileClient.publishWireEvent.mock.calls).toEqual([[{ operationId: "op-local" }]]);
    expect(gitClient.publishWireEvent.mock.calls).toEqual([[{ rootToken: "root-local" }]]);
    expect(agentClient.publishWireEvent).not.toHaveBeenCalled();
    await unmount();
  });

  it("stamps a link's agent events with its own host, server and client", async () => {
    const { agentClient, bridgeFor, unmount } = await twoShownHosts();
    const agentSnapshot = { connectionEpoch: 3, agents: [] } as never;
    await act(async () => {
      bridgeFor("remote-a").publish({ kind: "generationEpoch", epoch: 3, sequence: 0 });
      bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"]]));
      bridgeFor("remote-a").publish({ kind: "agentService", scope: "snapshot", snapshot: agentSnapshot, sequence: 0 });
      // A host that has not named its server yet has nothing to stamp with.
      bridgeFor("local").publish({ kind: "agentService", scope: "snapshot", snapshot: agentSnapshot, sequence: 0 });
    });
    expect(agentClient.publishWireSnapshot).toHaveBeenCalledTimes(1);
    const [scope, published] = agentClient.publishWireSnapshot.mock.calls[0];
    expect(scope).toMatchObject({ clientId: "client-2", hostProfileId: "remote-a", serverIdentity: "srv-qa", connectionEpoch: 3 });
    expect(published).toBe(agentSnapshot);
    await unmount();
  });

  it("restarts only the stalled peer's bridge, and only once, on a flow stall", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    const localEpoch = controller().linkFor("local")?.connectionEpoch;
    await act(async () => {
      bridgeFor("remote-a").publish({ kind: "flowStalled", paneId: "%0", message: "stalled", sequence: 0 });
      bridgeFor("remote-a").publish({ kind: "flowStalled", paneId: "%1", message: "stalled", sequence: 0 });
    });
    expect(stopTerminalMock.mock.calls).toEqual([["client-2"]]);
    expect(startTerminalMock).toHaveBeenCalledTimes(3);
    expect(bridgeFor("remote-a")).toMatchObject({ clientId: "client-3", attach: false });
    expect(controller().linkFor("local")?.connectionEpoch).toBe(localEpoch);
    expect(controller().linkFor("remote-a")?.detail).toBe("A terminal output stream stalled; reconnecting it now.");
    expect(controller().connectionDetail).toBe("");
    await unmount();
  });

  it("restarts exactly one bridge when the active host's address is corrected and connected", async () => {
    const { bridgeFor, controller, unmount } = await twoShownHosts();
    await act(async () => { controller().activateHost("remote-a"); });
    // What Settings' Connect does after an edit: the new address and a new
    // epoch, in one tick.
    await act(async () => {
      controller().setConnection({ mode: "ssh", profileId: "remote-a", target: "qa-host-2" });
      controller().setConnectionEpoch((value) => value + 1);
    });
    expect(stopTerminalMock.mock.calls).toEqual([["client-2"]]);
    expect(startTerminalMock).toHaveBeenCalledTimes(3);
    expect(bridgeFor("remote-a")).toMatchObject({
      clientId: "client-3", attach: true, connection: { mode: "ssh", profileId: "remote-a", target: "qa-host-2" },
    });
    expect(controller().linkFor("remote-a")?.connection).toEqual({ mode: "ssh", profileId: "remote-a", target: "qa-host-2" });
    expect(controller().links.map((link) => link.profileId)).toEqual(["local", "remote-a"]);
    await unmount();
  });

  it("reseeds a pane on its own host, forgetting only that host's cached screen", async () => {
    const { controller, unmount } = await twoShownHosts();
    const forgotten = vi.spyOn(terminalStateCache, "delete");
    await act(async () => { controller().hubFor("remote-a").onSeedRequired?.("%0", "test"); });
    expect(forgotten.mock.calls).toEqual([[terminalCacheKey("remote-a", "%0")]]);
    expect(requestTerminalSeedMock.mock.calls).toEqual([["client-2", "%0"]]);
    forgotten.mockRestore();
    await unmount();
  });

  it("stops a bridge whose start resolves after its host was un-shown, without adopting its client", async () => {
    const { controller, release, retireConnection, unmount } = await twoShownHosts({
      holdStart: (connection) => connection.mode === "ssh",
    });
    expect(controller().linkFor("remote-a")?.clientId).toBeUndefined();
    await act(async () => {
      controller().setProfiles((current) => current.map((profile) =>
        profile.id === "remote-a" ? { ...profile, shown: false } : profile));
    });
    // Nothing to stop yet: the native id is not known.
    expect(stopTerminalMock).not.toHaveBeenCalled();
    await act(async () => { release(); });
    expect(stopTerminalMock.mock.calls).toEqual([["client-2"]]);
    // No watch could have been registered on a client the shell never saw.
    expect(retireConnection).not.toHaveBeenCalled();
    expect(controller().links.map((link) => link.profileId)).toEqual(["local"]);
    expect(controller().clientId).toBe("client-1");
    await unmount();
  });

  it("reports transport transitions and handshake failures for the active host only", async () => {
    const states = vi.fn();
    const handshakes = vi.fn();
    const { bridgeFor, unmount } = await twoShownHosts({ onConnectionStateChanged: states, onHandshakeFailure: handshakes });
    await act(async () => {
      bridgeFor("remote-a").publish(connected);
      bridgeFor("remote-a").publish({ kind: "error", message: "host closed during handshake: No such file or directory", sequence: 0 });
      bridgeFor("local").publish(connected);
    });
    expect(states.mock.calls).toEqual([[{ mode: "local" }, "connected"]]);
    expect(handshakes).not.toHaveBeenCalled();
    expect(stopTerminalMock).not.toHaveBeenCalled();
    await unmount();
  });
});
