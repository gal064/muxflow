// @vitest-environment jsdom
// The shell over two shown hosts: what the sidebar draws, where a click on
// another host's row lands, and which client a row's mutation reaches.
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultAppState } from "../features/shell/types";
import { hostProfileId } from "../features/shell/types";
import type { TerminalEvent } from "../features/terminal/api";
import type { ConnectionSpec, HostProfile, TmuxSnapshot } from "./types";
import { App } from "./App";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn(async () => () => undefined));
const startTerminalMock = vi.hoisted(() => vi.fn());
const stopTerminalMock = vi.hoisted(() => vi.fn(async (_clientId: string) => undefined));
/** The host row anchors its menu under itself; the test renderer has no box to measure. */
const hostRowClick = { currentTarget: { getBoundingClientRect: () => ({ left: 0, bottom: 0 }) } };

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage?: (value: ArrayBuffer) => void },
  invoke: invokeMock,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/api/window", () => ({
  availableMonitors: vi.fn(async () => [{ position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 }, scaleFactor: 1 }]),
  getCurrentWindow: () => ({
    destroy: vi.fn(), innerSize: vi.fn(async () => ({ width: 1200, height: 800 })), isMaximized: vi.fn(async () => false), maximize: vi.fn(),
    onCloseRequested: vi.fn(async () => () => undefined), onMoved: vi.fn(async () => () => undefined), onResized: vi.fn(async () => () => undefined),
    outerPosition: vi.fn(async () => ({ x: 0, y: 0 })), scaleFactor: vi.fn(async () => 1), setPosition: vi.fn(async () => undefined), setSize: vi.fn(async () => undefined),
  }),
  primaryMonitor: vi.fn(async () => ({ position: { x: 0, y: 0 }, size: { width: 1920, height: 1080 }, scaleFactor: 1 })),
}));
// The bridges are not under test; which host each event comes from is.
vi.mock("../features/terminal/api", async (importOriginal) => ({
  ...await importOriginal<typeof import("../features/terminal/api")>(),
  startTerminal: startTerminalMock,
  stopTerminal: stopTerminalMock,
  requestTerminalSeed: vi.fn(async () => undefined),
}));

interface Bridge {
  clientId: string;
  connection: ConnectionSpec;
  attach: boolean;
  publish(event: TerminalEvent): void;
}

const LOCAL: HostProfile = { id: "local", label: "Local", connection: { mode: "local" }, shown: true };
const REMOTE: HostProfile = { id: "remote-a", label: "qa", connection: { mode: "ssh", profileId: "remote-a", target: "qa-host" }, shown: true };
const connected: TerminalEvent = { kind: "connectionState", state: "connected", sequence: 0 };
/** A server with one window and one pane per session, so rows resolve and agents can be routed. */
const world = (serverIdentity: string, sessions: Array<[id: string, name: string]>): TerminalEvent => {
  const snapshot: TmuxSnapshot = {
    sessions: sessions.map(([id, name], index) => ({ id, name, windowCount: 1, attachedClients: 0, order: index })),
    windows: sessions.map(([id, name], index) => ({ id: `@${index}`, sessionId: id, index: 0, name, active: true, layout: "" })),
    panes: sessions.map(([id], index) => ({
      id: `%${index}`, windowId: `@${index}`, sessionId: id, index: 0, active: true, width: 80, height: 24, left: 0, top: 0,
      currentPath: "/home/dev", currentCommand: "sh",
    })),
  };
  return { kind: "snapshot", snapshot, generation: 1, serverIdentity, authoritative: true, sequence: 0 };
};
const wireAgent = (lifecycle: "working" | "blocked", attentionGeneration: number) => ({
  agentId: "agent-qa", adapter: "codex", adapterId: "codex", nativeSessionId: "native-qa", displayName: "Codex on qa",
  route: {
    hostProfileId: "remote-a", serverIdentity: "srv-qa", sessionId: "$2", sessionNameFallback: "work",
    windowId: "@1", windowNameFallback: "work", paneId: "%1", agentId: "agent-qa", attentionGeneration,
  },
  lifecycle, stateGeneration: 1, attentionGeneration, ...(lifecycle === "blocked" ? { attentionKind: "blocked" } : {}),
  seenGeneration: 0, updatedAtUnixMillis: 100, lifecycleChangedAtUnixMillis: 100, attentionSeenAtUnixMillis: 0,
  detectedManually: false, present: true,
});
const calls = (command: string) => invokeMock.mock.calls.filter(([name]) => name === command).map(([, request]) => request);

async function shell(profiles: HostProfile[]) {
  const bridges: Bridge[] = [];
  startTerminalMock.mockImplementation(async (
    _sessionId: string, _paneIds: string[], connection: ConnectionSpec, attach: boolean, onEvent: (event: TerminalEvent) => void,
  ) => {
    const clientId = `client-${bridges.length + 1}`;
    bridges.push({ clientId, connection, attach, publish: onEvent });
    return clientId;
  });
  invokeMock.mockImplementation((command: string, request: Record<string, unknown>) => {
    switch (command) {
      case "load_app_state": return Promise.resolve(defaultAppState);
      case "list_host_profiles": return Promise.resolve({ schemaVersion: 1, lastProfileId: "local", profiles });
      case "tmux_action": return Promise.resolve({ topologyGeneration: 2 });
      case "agent_request": {
        const { connectionEpoch } = request.command as { connectionEpoch: string };
        return Promise.resolve({ snapshot: {
          generation: "1", acceptedGeneration: "1", authoritative: true, connectionEpoch, adapters: [],
          agents: request.clientId === "client-2" ? [wireAgent("working", 0)] : [],
        } });
      }
      case "emit_agent_notification": return Promise.resolve({ id: 1, actionable: true });
      case "bridge_final_totals": return Promise.resolve({ cumulativeFrameCount: 0, cumulativeByteLength: 0, quiesced: true });
      default: return Promise.resolve(undefined);
    }
  });
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(<App />); });
  const bridgeFor = (profileId: string) => {
    const bridge = [...bridges].reverse().find((candidate) => hostProfileId(candidate.connection) === profileId);
    if (!bridge) throw new Error(`no bridge for ${profileId}`);
    return bridge;
  };
  const buttons = () => renderer.root.findAllByType("button");
  const workspaceButton = (name: string) => buttons()
    .find((button) => String(button.props["aria-label"] ?? "").startsWith(`${name},`) || button.props["aria-label"] === name)!;
  const hostRow = () => buttons().find((button) => String(button.props["aria-label"] ?? "").startsWith("Host "))!;
  /** The workspace rows' letters, in list order; agent rows carry their own. */
  const letters = () => renderer.root.findAllByProps({ className: "workspace-title" })
    .flatMap((title) => title.findAllByProps({ className: "host-letter" }).map((node) => node.props.children as string));
  const menuItem = (id: string) => renderer.root.findByProps({ "data-menu-item": id });
  return {
    bridges,
    bridgeFor,
    renderer,
    workspaceButton,
    hostRow,
    letters,
    menuItem,
    openHostMenu: () => act(async () => { hostRow().props.onClick(hostRowClick); }),
    unmount: () => act(async () => renderer.unmount()),
  };
}

describe("the shell over several hosts", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockClear();
    startTerminalMock.mockReset();
    stopTerminalMock.mockClear();
    Object.assign(globalThis, {
      IS_REACT_ACT_ENVIRONMENT: true,
      ResizeObserver: class { observe() {} unobserve() {} disconnect() {} },
    });
  });

  it("draws letters only once a second host is shown, and starts and stops that host's bridge from the host menu", async () => {
    const app = await shell([LOCAL, { ...REMOTE, shown: false }]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
    });
    expect(app.bridges).toHaveLength(1);
    expect(app.letters()).toEqual([]);
    expect(app.workspaceButton("home").props["aria-label"]).toBe("home");

    // Checking the host in the menu saves it shown — and nothing else: the
    // last-profile pointer is the active host's.
    await app.openHostMenu();
    await act(async () => { app.menuItem("host-remote-a").props.onClick(); });
    expect(calls("save_host_profile")).toEqual([{ profile: { ...REMOTE, shown: true } }]);
    expect(calls("set_last_profile_id")).toEqual([]);
    expect(app.bridges.map((bridge) => [hostProfileId(bridge.connection), bridge.attach])).toEqual([["local", true], ["remote-a", false]]);
    await act(async () => {
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    expect(app.letters()).toEqual(["L", "Q", "Q"]);
    expect(app.workspaceButton("work").props["aria-label"]).toContain("work, on qa");
    // The host on screen keeps its row; only the peer's item can be toggled.
    await app.openHostMenu();
    expect(app.menuItem("host-local").props["aria-checked"]).toBe(true);
    expect(app.menuItem("host-local").props.disabled).toBe(true);

    // Unchecking it takes its bridge and its rows away.
    await act(async () => { app.menuItem("host-remote-a").props.onClick(); });
    expect(calls("save_host_profile").at(-1)).toEqual({ profile: { ...REMOTE, shown: false } });
    expect(stopTerminalMock.mock.calls).toEqual([["client-2"]]);
    expect(app.letters()).toEqual([]);
    expect(app.renderer.root.findAllByProps({ className: "workspace-row" })).toHaveLength(1);
    await app.unmount();
  });

  it("activates a peer host from one of its workspace rows, then selects that workspace there", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    expect(app.hostRow().props["aria-label"]).toContain("Host Local over local");

    await act(async () => { app.workspaceButton("work").props.onClick({ shiftKey: false }); });
    // The pointer moved and was persisted; the peer's own client was told
    // first about the session it remembered, then about the one clicked.
    expect(calls("set_last_profile_id")).toEqual([{ profileId: "remote-a" }]);
    expect(calls("select_terminal_session")).toContainEqual({ clientId: "client-2", sessionId: "$1" });
    expect(calls("tmux_action")).toContainEqual(expect.objectContaining({
      clientId: "client-2", action: expect.objectContaining({ kind: "selectSession", session_id: "$2" }),
    }));
    expect(calls("tmux_action").every((request) => request.clientId === "client-2")).toBe(true);
    expect(app.hostRow().props["aria-label"]).toContain("Host qa over ssh");
    expect(app.workspaceButton("work").props["aria-current"]).toBe("true");
    // No bridge was restarted for the switch.
    expect(app.bridges).toHaveLength(2);
    expect(stopTerminalMock).not.toHaveBeenCalled();
    await app.unmount();
  });

  it("renames a peer's workspace on the peer's client while the local host stays on screen", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    await act(async () => {
      app.workspaceButton("work").props.onContextMenu({ preventDefault() {}, clientX: 10, clientY: 10 });
    });
    await act(async () => { app.menuItem("rename").props.onClick(); });
    const input = app.renderer.root.findAllByType("input").find((node) => node.props.value === "work")!;
    await act(async () => { input.props.onChange({ target: { value: "work-renamed" } }); });
    await act(async () => { app.renderer.root.findByType("form").props.onSubmit({ preventDefault() {} }); });
    expect(calls("tmux_action")).toEqual([expect.objectContaining({
      clientId: "client-2",
      action: expect.objectContaining({ kind: "renameSession", session_id: "$2", name: "work-renamed" }),
    })]);
    expect(app.hostRow().props["aria-label"]).toContain("Host Local over local");
    await app.unmount();
  });

  it("pins a peer's workspace on the peer's client", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"]]));
    });
    await act(async () => { app.workspaceButton("build").props.onClick({ shiftKey: true }); });
    expect(calls("tmux_action")).toEqual([expect.objectContaining({
      clientId: "client-2", action: expect.objectContaining({ kind: "setPinned", session_id: "$1", pinned: true }),
    })]);
    expect(calls("set_last_profile_id")).toEqual([]);
    await app.unmount();
  });

  it("saves a host's letter and visibility from Settings, without Connect", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"]]));
    });
    await app.openHostMenu();
    await act(async () => { app.menuItem("settings").props.onClick(); });
    const letterField = () => app.renderer.root.findByProps({ "aria-label": "Host letter" });
    expect(letterField().props.placeholder).toBe("L");
    await act(async () => { letterField().props.onChange({ target: { value: "z" } }); });
    expect(calls("save_host_profile")).toEqual([{ profile: { ...LOCAL, letter: "z" } }]);
    expect(letterField().props.value).toBe("z");
    // Drawn upper-cased, on this host's rows only.
    expect(app.letters()).toEqual(["Z", "Q"]);
    // The host on screen cannot be hidden from here either.
    const shown = app.renderer.root.findAllByType("input").find((node) => node.props.type === "checkbox" && node.props.disabled)!;
    expect(shown.props.checked).toBe(true);
    expect(calls("set_last_profile_id")).toEqual([]);
    expect(app.bridges).toHaveLength(2);
    await app.unmount();
  });

  it("resolves a clicked notification from a peer on that peer, after activating it", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    const activation = (listenMock.mock.calls as unknown as Array<[string, (event: { payload: unknown }) => void]>)
      .find(([event]) => event === "notification-activated");
    expect(activation).toBeDefined();
    await act(async () => {
      activation?.[1]({ payload: {
        hostProfile: "remote-a", serverIdentity: "srv-qa", sessionId: "$2", sessionName: "work",
        windowId: "@1", windowName: "work", paneId: "%1", agentId: "agent-qa", attentionGeneration: "1",
      } });
    });
    expect(calls("set_last_profile_id")).toEqual([{ profileId: "remote-a" }]);
    expect(app.hostRow().props["aria-label"]).toContain("Host qa over ssh");
    expect(calls("resolve_notification_route")).toEqual([expect.objectContaining({
      route: expect.objectContaining({ hostProfile: "remote-a", paneId: "%1" }),
    })]);
    await app.unmount();
  });

  it("lists a peer's agents with its letter, and still notifies for them", async () => {
    const app = await shell([LOCAL, REMOTE]);
    await act(async () => {
      app.bridgeFor("local").publish(connected);
      app.bridgeFor("local").publish(world("srv-local", [["$0", "home"]]));
      app.bridgeFor("remote-a").publish({ kind: "generationEpoch", epoch: 3, sequence: 0 });
      app.bridgeFor("remote-a").publish(connected);
      app.bridgeFor("remote-a").publish(world("srv-qa", [["$1", "build"], ["$2", "work"]]));
    });
    // Each host's scope requested its own snapshot, under its own client.
    await act(async () => { await Promise.resolve(); });
    expect(calls("agent_request").map((request) => request.clientId)).toEqual(["client-1", "client-2"]);
    const agentButton = app.renderer.root.findAllByType("button")
      .find((button) => String(button.props.title ?? "").includes("Codex on qa"))!;
    expect(agentButton.props["aria-label"]).toContain("work, qa");
    const agentLine = agentButton.findByProps({ className: "agent-line" });
    expect(agentLine.findAllByProps({ className: "host-letter" }).map((node) => node.props.children)).toEqual(["Q"]);

    await act(async () => {
      app.bridgeFor("remote-a").publish({
        kind: "agentService", scope: "event", sequence: 0,
        event: { agent: wireAgent("blocked", 1), generation: "2", connectionEpoch: 3, notify: true },
      } as TerminalEvent);
    });
    expect(calls("emit_agent_notification")).toEqual([expect.objectContaining({
      notification: expect.objectContaining({
        title: "Codex on qa needs attention",
        route: expect.objectContaining({ hostProfile: "remote-a", sessionId: "$2", paneId: "%1" }),
      }),
    })]);

    // The bell reaches it: the host is activated, then its pane surfaced.
    const bell = app.renderer.root.findAllByType("button")
      .find((button) => String(button.props["aria-label"] ?? "").startsWith("1 agent waiting"))!;
    expect(bell.props.disabled).toBeFalsy();
    await act(async () => { bell.props.onClick(); });
    expect(calls("set_last_profile_id")).toEqual([{ profileId: "remote-a" }]);
    expect(calls("tmux_action")).toContainEqual(expect.objectContaining({
      clientId: "client-2", action: expect.objectContaining({ session_id: "$2" }),
    }));
    await app.unmount();
  });
});
