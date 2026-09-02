// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { TmuxActionResult } from "../features/tmux/actions";
import type { OptimisticWindowSwitch } from "./windowSelection";
import {
  commitScopedAppTabClose,
  RemoteNavigationCoordinator,
  reportAnnouncedPaneResult,
  ShellNavigationSupersededError,
  shellTransitionPlan,
  useShellNavigation,
  type ShellDestination,
  type ShellNavigationOptions,
} from "./useShellNavigation";

function deferred<T>() {
  let resolve!: (result: T) => void;
  return { promise: new Promise<T>((done) => { resolve = done; }), resolve };
}

const destination = (id: string): ShellDestination => ({ kind: "session", sessionId: id });
const intent = (id: string, request: () => Promise<boolean>, commit = vi.fn()) => ({
  destination: destination(id),
  request: async () => await request()
    ? { kind: "reached" as const, destination: destination(id), generation: 1, generationSource: "action" as const }
    : { kind: "unknown" as const, reason: "request" as const },
  commit,
});
const flush = async () => { await act(async () => { await new Promise((resolve) => globalThis.setTimeout(resolve, 0)); }); };

describe("serialized remote navigation", () => {
  it("commits a no-flight local destination without inventing a remote outcome", () => {
    const coordinator = new RemoteNavigationCoordinator();
    const commit = vi.fn();
    expect(coordinator.navigateLocal(intent("local", async () => true, commit))).toBeUndefined();
    expect(commit).toHaveBeenCalledOnce();
  });

  it("settles an accepted unsuperseded destination as accepted", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const commit = vi.fn();
    await expect(coordinator.navigate(intent("only", async () => true, commit))).resolves.toMatchObject({ kind: "reached" });
    expect(commit).toHaveBeenCalledOnce();
  });

  it("dispatches A then only the latest C for a rapid A→B→C intent sequence", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const a = deferred<boolean>();
    const c = deferred<boolean>();
    const requestA = vi.fn(() => a.promise);
    const requestB = vi.fn(async () => true);
    const requestC = vi.fn(() => c.promise);
    const commit = [vi.fn(), vi.fn(), vi.fn()];
    void coordinator.navigate(intent("A", requestA, commit[0]));
    void coordinator.navigate(intent("B", requestB, commit[1]));
    void coordinator.navigate(intent("C", requestC, commit[2]));
    expect([requestA.mock.calls.length, requestB.mock.calls.length, requestC.mock.calls.length]).toEqual([1, 0, 0]);
    a.resolve(true);
    await flush();
    expect([requestA.mock.calls.length, requestB.mock.calls.length, requestC.mock.calls.length]).toEqual([1, 0, 1]);
    c.resolve(true);
    await flush();
    expect(commit.map((callback) => callback.mock.calls.length)).toEqual([0, 0, 1]);
  });

  it("honors returning to the currently rendered target while another selection is pending", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const away = deferred<boolean>();
    const back = deferred<boolean>();
    const commitBack = vi.fn();
    void coordinator.navigate(intent("S1", () => away.promise));
    void coordinator.navigate(intent("S0", () => back.promise, commitBack), {
      kind: "reached", destination: destination("S0"), generation: 1, generationSource: "snapshot",
    });
    away.resolve(true);
    await flush();
    back.resolve(true);
    await flush();
    expect(commitBack).toHaveBeenCalledOnce();
  });

  it("lets a local intent win over a pending remote completion and reasserts it", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const remote = deferred<boolean>();
    const localReassert = deferred<boolean>();
    const remoteCommit = vi.fn();
    const localCommit = vi.fn();
    void coordinator.navigate(intent("remote", () => remote.promise, remoteCommit));
    void coordinator.navigateLocal(intent("local", () => localReassert.promise, localCommit));
    expect(localCommit).toHaveBeenCalledOnce();
    remote.resolve(true);
    await flush();
    localReassert.resolve(true);
    await flush();
    expect(localCommit).toHaveBeenCalledTimes(2);
    expect(remoteCommit).not.toHaveBeenCalled();
  });

  it("coalesces an exact in-flight duplicate and uses its latest commit", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const pending = deferred<boolean>();
    const request = vi.fn(() => pending.promise);
    const firstCommit = vi.fn();
    const latestCommit = vi.fn();
    void coordinator.navigate(intent("same", request, firstCommit));
    void coordinator.navigate(intent("same", request, latestCommit));
    pending.resolve(true);
    await flush();
    expect(request).toHaveBeenCalledOnce();
    expect(firstCommit).not.toHaveBeenCalled();
    expect(latestCommit).toHaveBeenCalledOnce();
  });

  it("detaches old work on connection invalidation and admits the replacement immediately", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const old = deferred<boolean>();
    const replacement = deferred<boolean>();
    const oldCommit = vi.fn();
    const replacementCommit = vi.fn();
    void coordinator.navigate(intent("old", () => old.promise, oldCommit));
    coordinator.invalidate();
    void coordinator.navigate(intent("replacement", () => replacement.promise, replacementCommit));
    old.resolve(true);
    replacement.resolve(true);
    await flush();
    expect(oldCommit).not.toHaveBeenCalled();
    expect(replacementCommit).toHaveBeenCalledOnce();
  });

  it("continues to the latest queued destination after a rejected request", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const latest = deferred<boolean>();
    const latestRequest = vi.fn(() => latest.promise);
    const latestCommit = vi.fn();
    void coordinator.navigate(intent("broken", async () => { throw new Error("offline"); }));
    const accepted = coordinator.navigate(intent("latest", latestRequest, latestCommit));
    await flush();
    expect(latestRequest).toHaveBeenCalledOnce();
    latest.resolve(true);
    await expect(accepted).resolves.toMatchObject({ kind: "reached" });
    expect(latestCommit).toHaveBeenCalledOnce();
  });
});

describe("shell transition planning", () => {
  const reached = (target: ShellDestination) => ({
    kind: "reached" as const, destination: target, generation: 1, generationSource: "action" as const,
  });

  it("uses only the writes needed from the typed predecessor outcome", () => {
    expect(shellTransitionPlan(
      reached({ kind: "window", sessionId: "$1", windowId: "@0" }),
      { kind: "window", sessionId: "$1", windowId: "@1" },
    )).toEqual({ selectSession: false, selectWindow: true });
    expect(shellTransitionPlan(
      reached({ kind: "pane", sessionId: "$1", windowId: "@0", paneId: "%1" }),
      { kind: "pane", sessionId: "$1", windowId: "@0", paneId: "%2" },
    )).toEqual({ selectSession: false, selectWindow: false });
    expect(shellTransitionPlan(
      reached({ kind: "window", sessionId: "$2", windowId: "@9" }),
      { kind: "window", sessionId: "$1", windowId: "@0" },
    )).toEqual({ selectSession: true, selectWindow: true });
    expect(shellTransitionPlan(
      { kind: "unknown", reason: "request" },
      { kind: "window", sessionId: "$1", windowId: "@0" },
    )).toEqual({ selectSession: true, selectWindow: true });
    expect(shellTransitionPlan(
      reached({ kind: "window", sessionId: "$1", windowId: "@0" }),
      { kind: "appTab", sessionId: "$1", windowId: "@0", appTabId: "notes" },
    )).toEqual({ selectSession: false, selectWindow: false });
    expect(shellTransitionPlan(
      reached({ kind: "window", sessionId: "$1", windowId: "@0" }),
      { kind: "pane", sessionId: "$2", windowId: "@9", paneId: "%9" },
      true,
    )).toEqual({ selectSession: true, selectWindow: false });
    expect(shellTransitionPlan(
      reached({ kind: "session", sessionId: "$2" }),
      { kind: "pane", sessionId: "$2", windowId: "@9", paneId: "%9" },
      true,
    )).toEqual({ selectSession: false, selectWindow: false });
    expect(shellTransitionPlan(
      reached({ kind: "window", sessionId: "$2", windowId: "@8" }),
      { kind: "pane", sessionId: "$2", windowId: "@9", paneId: "%9" },
      true,
    )).toEqual({ selectSession: false, selectWindow: true });
  });
});

describe("announced pane result consumption", () => {
  it("reports hard failures without turning superseded navigation into noise", () => {
    const setStatus = vi.fn();
    reportAnnouncedPaneResult({ ok: false, error: new Error("Agent focus request was not accepted.") }, setStatus);
    reportAnnouncedPaneResult({ ok: false, error: new ShellNavigationSupersededError() }, setStatus);
    expect(setStatus).toHaveBeenCalledOnce();
  });
});

describe("app-tab close consumption", () => {
  it("closes the old tab without revealing a terminal after selection moved to a newer app tab", () => {
    const commit = vi.fn();
    const revealTerminal = vi.fn();
    commitScopedAppTabClose({
      activeWindowId: "@0", commit, currentScope: scope, revealTerminal, scope,
      selectedAppTabId: "new-tab", tabId: "old-tab", tabSessionId: "$1",
    });
    expect(commit).toHaveBeenCalledOnce();
    expect(revealTerminal).not.toHaveBeenCalled();
  });
});

const scope = {
  hostProfileId: "remote", connectionKey: "ssh:remote", connectionEpoch: 1,
  serverIdentity: "server-a", generation: 1,
};
const sessions = [
  { id: "$1", name: "one", windowCount: 2, attachedClients: 1 },
  { id: "$2", name: "two", windowCount: 1, attachedClients: 1 },
];
const windows = [
  { id: "@0", sessionId: "$1", index: 0, name: "zero", active: true, layout: "" },
  { id: "@1", sessionId: "$1", index: 1, name: "one", active: false, layout: "" },
];

function mountNavigation(overrides: Partial<ShellNavigationOptions> = {}) {
  const setActiveSessionId = vi.fn();
  const setActiveWindowId = vi.fn();
  const setAppTab = vi.fn();
  const setPendingTab = vi.fn();
  const setStatus = vi.fn();
  const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 2 }));
  let navigation!: ReturnType<typeof useShellNavigation>;
  const options = (extra: Partial<ShellNavigationOptions> = {}): ShellNavigationOptions => ({
    activeSessionId: "$1",
    activeWindowId: "@0",
    canMutate: true,
    currentScope: scope,
    focusPaneController: vi.fn(),
    performAction,
    sessions,
    setActiveSessionId,
    setActiveWindowId,
    setAppTab,
    setPendingTab,
    setStatus,
    windows,
    ...overrides,
    ...extra,
  });
  function Harness(props: { extra?: Partial<ShellNavigationOptions> }) {
    navigation = useShellNavigation(options(props.extra));
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  return {
    get navigation() { return navigation; },
    performAction,
    renderer: async () => {
      await act(async () => { renderer = create(<Harness />); });
      return renderer;
    },
    rerender: async (extra: Partial<ShellNavigationOptions>) => {
      await act(async () => { renderer.update(<Harness extra={extra} />); });
    },
    setActiveSessionId,
    setActiveWindowId,
    setAppTab,
    setPendingTab,
  };
}

describe("shell navigation hook cross-kind ownership", () => {
  it("keeps one controller identity across unrelated root rerenders", async () => {
    const harness = mountNavigation();
    const renderer = await harness.renderer();
    const initial = harness.navigation;

    await harness.rerender({ setStatus: vi.fn() });

    expect(harness.navigation).toBe(initial);
    await act(async () => renderer.unmount());
  });

  it("uses the current epoch-scoped acknowledgement after an ack-only rerender", async () => {
    const oldEpochAck = vi.fn();
    const newEpochAck = vi.fn();
    const harness = mountNavigation({ acknowledgeHostSessionSelection: oldEpochAck });
    const renderer = await harness.renderer();
    await harness.rerender({ acknowledgeHostSessionSelection: newEpochAck });

    act(() => harness.navigation.selectSession("$2"));
    await flush();
    expect(oldEpochAck).not.toHaveBeenCalled();
    expect(newEpochAck).toHaveBeenCalledWith("$2");
    await act(async () => renderer.unmount());
  });

  it("sends a post-commit reversal before any authoritative snapshot rerender", async () => {
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "selectSession" && action.sessionId) actions.push(action.sessionId);
      return { topologyGeneration: actions.length + 1 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    await flush();
    act(() => harness.navigation.selectSession("$1"));
    await flush();
    expect(actions).toEqual(["$2", "$1"]);
    await act(async () => renderer.unmount());
  });

  it("switches window-to-window in one RTT when the predecessor already selected the session", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      actions.push(action.kind === "selectWindow" ? `${action.kind}:${action.windowId}` : action.kind);
      if (action.kind === "selectWindow" && action.windowId === "@1") return first.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectWindow("@1"));
    act(() => harness.navigation.selectWindow("@0"));
    first.resolve({ topologyGeneration: 2 });
    await flush();
    expect(actions).toEqual(["selectWindow:@1", "selectWindow:@0"]);
    expect(performAction.mock.calls[0]?.[1]).toBeUndefined();
    expect(performAction.mock.calls[1]?.[1]).toEqual({ serverIdentity: "server-a", generation: 2 });
    await act(async () => renderer.unmount());
  });

  it("moves pane-to-pane in one window without redundant session or window writes", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      actions.push(action.kind === "focusPane" ? `${action.kind}:${action.paneId}` : action.kind);
      if (action.kind === "focusPane" && action.paneId === "%1") return first.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    const pane = (id: string) => ({
      id, sessionId: "$1", windowId: "@0", index: 0, active: false,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    });
    void harness.navigation.selectPane(pane("%1"), { kind: "silent" });
    await flush();
    void harness.navigation.selectPane(pane("%2"), { kind: "silent" });
    first.resolve({ topologyGeneration: 2 });
    await flush();
    expect(actions).toEqual(["focusPane:%1", "focusPane:%2"]);
    expect(performAction.mock.calls[1]?.[1]).toEqual({ serverIdentity: "server-a", generation: 2 });
    await act(async () => renderer.unmount());
  });

  it("selects the session before focusing its active pane from another session", async () => {
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      actions.push(action.kind);
      return { topologyGeneration: action.kind === "selectSession" ? 2 : 3 };
    });
    const harness = mountNavigation({
      performAction,
      windows: [...windows, { id: "@9", sessionId: "$2", index: 0, name: "other", active: true, layout: "" }],
    });
    const renderer = await harness.renderer();
    await act(async () => {
      await harness.navigation.selectPane({
        id: "%9", sessionId: "$2", windowId: "@9", index: 0, active: true,
        width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
      }, { kind: "silent" });
    });
    expect(actions).toEqual(["selectSession", "focusPane"]);
    expect(performAction.mock.calls[1]?.[1]).toEqual({ serverIdentity: "server-a", generation: 2 });
    await act(async () => renderer.unmount());
  });

  it("chains session, inactive window, and pane generations across workspaces", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => ({
      topologyGeneration: action.kind === "selectSession" ? 2 : action.kind === "selectWindow" ? 3 : 4,
    }));
    const harness = mountNavigation({
      performAction,
      windows: [...windows, { id: "@9", sessionId: "$2", index: 1, name: "other", active: false, layout: "" }],
    });
    const renderer = await harness.renderer();
    await act(async () => {
      await harness.navigation.selectPane({
        id: "%9", sessionId: "$2", windowId: "@9", index: 0, active: true,
        width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
      }, { kind: "silent" });
    });
    expect(performAction.mock.calls.map(([action]) => action.kind)).toEqual(["selectSession", "selectWindow", "focusPane"]);
    expect(performAction.mock.calls[0]?.[1]).toBeUndefined();
    expect(performAction.mock.calls[1]?.[1]).toEqual({ serverIdentity: "server-a", generation: 2 });
    expect(performAction.mock.calls[2]?.[1]).toEqual({ serverIdentity: "server-a", generation: 3 });
    await act(async () => renderer.unmount());
  });

  it("uses only a session handoff before an active-window pane queued from another workspace", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      actions.push(action.kind);
      if (action.kind === "selectSession" && action.sessionId === "$2") return first.promise;
      return { topologyGeneration: actions.length + 1 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    void harness.navigation.selectPane({
      id: "%1", sessionId: "$1", windowId: "@0", index: 0, active: true,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "silent" });
    first.resolve({ topologyGeneration: 2 });
    await flush();
    expect(actions).toEqual(["selectSession", "selectSession", "focusPane"]);
    expect(performAction.mock.calls[2]?.[1]).toEqual({ serverIdentity: "server-a", generation: 3 });
    await act(async () => renderer.unmount());
  });

  it("does not reselect the active window for a pane queued behind its session", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "selectSession") return first.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({
      performAction,
      windows: [...windows, { id: "@9", sessionId: "$2", index: 0, name: "other", active: true, layout: "" }],
    });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    const paneResult = harness.navigation.selectPane({
      id: "%9", sessionId: "$2", windowId: "@9", index: 0, active: true,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "silent" });
    first.resolve({ topologyGeneration: 2 });
    await expect(paneResult).resolves.toEqual({ ok: true });
    expect(performAction.mock.calls.map(([action]) => action.kind)).toEqual(["selectSession", "focusPane"]);
    expect(performAction.mock.calls[1]?.[1]).toEqual({ serverIdentity: "server-a", generation: 2 });
    await act(async () => renderer.unmount());
  });

  it("does not reuse an untrusted generation after failed pane focus", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "focusPane" && action.paneId === "%1") return first.promise;
      return { topologyGeneration: action.kind === "selectSession" ? 3 : action.kind === "selectWindow" ? 4 : 5 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    const pane = (id: string) => ({
      id, sessionId: "$1", windowId: "@0", index: 0, active: false,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    });
    void harness.navigation.selectPane(pane("%1"), { kind: "silent" });
    await flush();
    void harness.navigation.selectPane(pane("%2"), { kind: "silent" });
    first.resolve(undefined);
    await flush();
    expect(performAction.mock.calls.map(([action]) => action.kind)).toEqual([
      "focusPane", "selectSession", "selectWindow", "focusPane",
    ]);
    expect(performAction.mock.calls[1]?.[1]).toBeUndefined();
    expect(performAction.mock.calls[2]?.[1]).toEqual({ serverIdentity: "server-a", generation: 3 });
    expect(performAction.mock.calls[3]?.[1]).toEqual({ serverIdentity: "server-a", generation: 4 });
    await act(async () => renderer.unmount());
  });

  it("restores both session and window for a window intent queued behind a workspace", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    const restoreSession = deferred<TmuxActionResult | undefined>();
    const restoreWindow = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const harness = mountNavigation({
      performAction: vi.fn(async (action) => {
        actions.push(action.kind === "selectSession" ? `${action.kind}:${action.sessionId}`
          : action.kind === "selectWindow" ? `${action.kind}:${action.windowId}` : action.kind);
        if (action.kind === "selectSession" && action.sessionId === "$2") return first.promise;
        if (action.kind === "selectSession") return restoreSession.promise;
        return restoreWindow.promise;
      }),
    });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectWindow("@1"));
    first.resolve({ topologyGeneration: 2 });
    await flush();
    restoreSession.resolve({ topologyGeneration: 3 });
    await flush();
    restoreWindow.resolve({ topologyGeneration: 4 });
    await flush();
    expect(harness.setActiveSessionId).toHaveBeenLastCalledWith("$1");
    expect(harness.setActiveSessionId).not.toHaveBeenCalledWith("$2");
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@1");
    expect(actions).toEqual(["selectSession:$2", "selectSession:$1", "selectWindow:@1"]);
    await act(async () => renderer.unmount());
  });

  it("clears the selected document tab when the already-active terminal tab is picked", async () => {
    // The path a click on a terminal tab takes while a Git diff is on screen.
    // The window is the one tmux is already on, so there is nothing to ask the
    // host for — and the selection still has to be cleared, because that is
    // what uncovers the terminal. The diff's own tab is untouched: only the
    // workspace's selection moves, so the tab stays in the strip.
    const harness = mountNavigation();
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectAppTab("$1", "@0", "diff"));
    expect(harness.setAppTab).toHaveBeenLastCalledWith("$1", "diff");

    act(() => harness.navigation.selectWindow("@0"));
    await flush();
    expect(harness.setAppTab).toHaveBeenLastCalledWith("$1", undefined);
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@0");
    // Nothing was sent: tmux is already on this window.
    expect(harness.performAction).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("reasserts a local app tab after an older remote selection", async () => {
    const remote = deferred<TmuxActionResult | undefined>();
    const restoreSession = deferred<TmuxActionResult | undefined>();
    const restoreWindow = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      actions.push(action.kind === "selectSession" ? `${action.kind}:${action.sessionId}`
        : action.kind === "selectWindow" ? `${action.kind}:${action.windowId}` : action.kind);
      if (action.kind === "selectSession" && action.sessionId === "$2") return remote.promise;
      if (action.kind === "selectSession") return restoreSession.promise;
      return restoreWindow.promise;
    });
    const harness = mountNavigation({
      performAction,
    });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectAppTab("$1", "@0", "notes"));
    expect(harness.setAppTab).toHaveBeenLastCalledWith("$1", "notes");
    remote.resolve({ topologyGeneration: 2 });
    await flush();
    restoreSession.resolve({ topologyGeneration: 3 });
    await flush();
    restoreWindow.resolve({ topologyGeneration: 4 });
    await flush();
    expect(harness.setAppTab.mock.calls.filter((call) => call[1] === "notes")).toHaveLength(2);
    expect(harness.setActiveSessionId).not.toHaveBeenCalledWith("$2");
    expect(actions).toEqual(["selectSession:$2", "selectSession:$1"]);
    expect(performAction.mock.calls[1]?.[2]).toEqual({
      kind: "navigation", feedback: "silent", measurePanePaint: false,
    });
    await act(async () => renderer.unmount());
  });

  it("protects an app tab through stale and final authoritative snapshots from an older window flight", async () => {
    const remote = deferred<TmuxActionResult | undefined>();
    const restore = deferred<TmuxActionResult | undefined>();
    let call = 0;
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => {
      call += 1;
      return call === 1 ? remote.promise : restore.promise;
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    let visibleAppTab: string | undefined = "notes";
    const observe = (windowId: string, generation: number) => {
      if (!harness.navigation.observeAuthoritativeWindow("$1", windowId, generation)) visibleAppTab = undefined;
    };
    act(() => harness.navigation.selectWindow("@1"));
    act(() => harness.navigation.selectAppTab("$1", "@0", "notes"));
    observe("@1", 2);
    expect(visibleAppTab).toBe("notes");
    remote.resolve({ topologyGeneration: 2 });
    await flush();
    // The production host queues the matching topology snapshot before it
    // resolves the action response. Remember it so response completion can
    // release protection without requiring an extra snapshot.
    observe("@0", 3);
    expect(visibleAppTab).toBe("notes");
    restore.resolve({ topologyGeneration: 3 });
    await flush();
    observe("@1", 4);
    expect(visibleAppTab).toBeUndefined();
    expect(performAction.mock.calls[1]?.[2]).toEqual({
      kind: "navigation", feedback: "silent", measurePanePaint: false,
    });
    await act(async () => renderer.unmount());
  });

  it.each(["file", "diff"])("lets a newly opened %s tab supersede an older remote flight", async (kind) => {
    const remote = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) =>
      (kind === "file" && action.kind === "selectWindow" && action.windowId === "@1")
        || (kind === "diff" && action.kind === "selectSession" && action.sessionId === "$2")
        ? remote.promise
        : { topologyGeneration: 3 });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    const open = vi.fn();
    act(() => kind === "file" ? harness.navigation.selectWindow("@1") : harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectLocalAppTab("$1", "@0", `${kind}:resource`, open));
    expect(open).toHaveBeenCalledOnce();
    remote.resolve({ topologyGeneration: 2 });
    await flush();
    expect(open).toHaveBeenCalledTimes(2);
    expect(harness.setAppTab).not.toHaveBeenCalledWith("$1", undefined);
    await act(async () => renderer.unmount());
  });

  it("cancels a pending app reassertion when the selected app tab closes", async () => {
    const remote = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) =>
      action.kind === "selectSession" && action.sessionId === "$2"
        ? remote.promise
        : { topologyGeneration: 3 });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    const open = vi.fn();
    const close = vi.fn();
    act(() => harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectLocalAppTab("$1", "@0", "file:notes", open));
    act(() => harness.navigation.revealLocalTerminal("$1", "@0", close));
    expect(open).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
    remote.resolve({ topologyGeneration: 2 });
    await flush();
    expect(open).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledTimes(2);
    await act(async () => renderer.unmount());
  });

  it("re-establishes the full location after an unknown predecessor outcome", async () => {
    const failed = deferred<TmuxActionResult | undefined>();
    const actions: string[] = [];
    const harness = mountNavigation({
      performAction: vi.fn(async (action) => {
        actions.push(action.kind === "selectSession" ? `${action.kind}:${action.sessionId}`
          : action.kind === "selectWindow" ? `${action.kind}:${action.windowId}` : action.kind);
        if (action.kind === "selectSession" && action.sessionId === "$2") return failed.promise;
        return { topologyGeneration: 3 };
      }),
    });
    const renderer = await harness.renderer();
    act(() => harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectWindow("@1"));
    failed.resolve(undefined);
    await flush();
    expect(actions).toEqual(["selectSession:$2", "selectSession:$1", "selectWindow:@1"]);
    await act(async () => renderer.unmount());
  });

  it("keeps routine manual pane focus silent and uninstrumented", async () => {
    const setStatus = vi.fn();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 2 }));
    const harness = mountNavigation({ performAction, setStatus });
    const renderer = await harness.renderer();
    await act(async () => {
      await harness.navigation.selectPane({
        id: "%9", sessionId: "$1", windowId: "@0", index: 0, active: false,
        width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
      }, { kind: "silent" });
    });
    expect(setStatus).not.toHaveBeenCalled();
    expect(performAction).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "focusPane", paneId: "%9" }),
      undefined,
      { kind: "navigation", feedback: "silent", measurePanePaint: false },
    );
    await act(async () => renderer.unmount());
  });

  it("preserves a stale focus cause for announced notification retry", async () => {
    const stale = new Error("stale topology generation");
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "focusPane") throw stale;
      return { topologyGeneration: 2 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    const result = await harness.navigation.selectPane({
      id: "%9", sessionId: "$1", windowId: "@0", index: 0, active: false,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "announce", source: "Notification" });
    expect(result).toEqual({ ok: false, error: stale });
    await act(async () => renderer.unmount());
  });

  it("preserves a stale chained-window cause for notification retry", async () => {
    const stale = new Error("stale topology generation");
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "selectSession") return { topologyGeneration: 2 };
      if (action.kind === "selectWindow") throw stale;
      return { topologyGeneration: 4 };
    });
    const harness = mountNavigation({
      performAction,
      windows: [...windows, { id: "@9", sessionId: "$2", index: 1, name: "other", active: false, layout: "" }],
    });
    const renderer = await harness.renderer();
    const result = await harness.navigation.selectPane({
      id: "%9", sessionId: "$2", windowId: "@9", index: 0, active: false,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "announce", source: "Notification" });
    expect(result).toEqual({ ok: false, error: stale });
    expect(performAction.mock.calls.map(([action]) => action.kind)).toEqual(["selectSession", "selectWindow"]);
    await act(async () => renderer.unmount());
  });

  it("does not send pane focus after scope replacement during window selection", async () => {
    const selectedWindow = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "selectWindow") return selectedWindow.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    void harness.navigation.selectPane({
      id: "%9", sessionId: "$1", windowId: "@1", index: 0, active: true,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "announce", source: "Agent" });
    await harness.rerender({ currentScope: { ...scope, connectionEpoch: 2 } });
    selectedWindow.resolve({ topologyGeneration: 2 });
    await flush();
    expect(performAction.mock.calls.some(([action]) => action.kind === "focusPane")).toBe(false);
    await act(async () => renderer.unmount());
  });

  it("does not send pane focus after a newer app-tab intent arrives during window selection", async () => {
    const selectedWindow = deferred<TmuxActionResult | undefined>();
    let firstWindow = true;
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "selectWindow" && firstWindow) {
        firstWindow = false;
        return selectedWindow.promise;
      }
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    void harness.navigation.selectPane({
      id: "%9", sessionId: "$1", windowId: "@1", index: 0, active: true,
      width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
    }, { kind: "announce", source: "Agent" });
    act(() => harness.navigation.selectAppTab("$1", "@0", "notes"));
    selectedWindow.resolve({ topologyGeneration: 2 });
    await flush();
    await flush();
    expect(performAction.mock.calls.some(([action]) => action.kind === "focusPane")).toBe(false);
    expect(harness.setAppTab).toHaveBeenLastCalledWith("$1", "notes");
    await act(async () => renderer.unmount());
  });

  it("lets a newer app-tab intent supersede an in-flight create destination", async () => {
    const created = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") return created.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    act(() => harness.navigation.createWindow("$1"));
    act(() => harness.navigation.selectAppTab("$1", "@0", "notes"));
    created.resolve({ sessionId: "$1", windowId: "@9", topologyGeneration: 2 });
    await flush();
    await flush();
    expect(performAction.mock.calls[0]?.[0]).toEqual({ kind: "createWindow", sessionId: "$1" });
    expect(harness.setAppTab).toHaveBeenLastCalledWith("$1", "notes");
    await act(async () => renderer.unmount());
  });
});

describe("creating a window over a document tab", () => {
  it("selects the created window in the same workspace and only deselects the document", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") return { sessionId: "$1", windowId: "@9", topologyGeneration: 2 };
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();
    act(() => harness.navigation.createWindow("$1"));
    await flush();
    expect(performAction).toHaveBeenCalledExactlyOnceWith({ kind: "createWindow", sessionId: "$1" });
    expect(harness.setAppTab).toHaveBeenCalledExactlyOnceWith("$1", undefined);
    expect(harness.setActiveSessionId).toHaveBeenLastCalledWith("$1");
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@9");
    await act(async () => renderer.unmount());
  });
});

describe("pending tab placeholder on create", () => {
  it("puts a placeholder up before the create round trip is answered", async () => {
    const created = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") return created.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createWindow("$1"));
    const placeholder = harness.setPendingTab.mock.calls[0]?.[0];
    expect(
      placeholder,
      "the strip has to change in the same frame as the click, not a round trip later",
    ).toMatchObject({ sessionId: "$1" });
    expect(placeholder?.windowId, "there is no window to name until the ack").toBeUndefined();

    created.resolve({ sessionId: "$1", windowId: "@9", topologyGeneration: 2 });
    await flush();
    await act(async () => renderer.unmount());
  });

  /**
   * The ack hands the placeholder its window id and nothing else.
   *
   * Retirement is not this hook's to do: it happens in the shell, on the
   * snapshot that names the window, and once (`retirePendingTab`). What this
   * has to guarantee is that the successful path never publishes `undefined` —
   * neither as a withdrawal, which would blink the strip empty between the ack
   * and the snapshot, nor as a retirement it cannot make stick.
   */
  it("upgrades the placeholder with the real window id and leaves retirement to the shell", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") return { sessionId: "$1", windowId: "@9", topologyGeneration: 2 };
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createWindow("$1"));
    await flush();

    expect(harness.setPendingTab).toHaveBeenLastCalledWith(
      expect.objectContaining({ sessionId: "$1", windowId: "@9" }),
    );
    expect(
      harness.setPendingTab.mock.calls.map(([pending]) => pending),
      "a create that succeeded never withdraws its own placeholder",
    ).not.toContain(undefined);
    await act(async () => renderer.unmount());
  });

  it("rolls the placeholder back when the create is refused", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") return undefined;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createWindow("$1"));
    await flush();

    expect(harness.setPendingTab).toHaveBeenLastCalledWith(undefined);
    await act(async () => renderer.unmount());
  });

  it("rolls the placeholder back when the create throws", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createWindow") throw new Error("host went away");
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createWindow("$1"));
    await flush();

    expect(harness.setPendingTab).toHaveBeenLastCalledWith(undefined);
    await act(async () => renderer.unmount());
  });

  it("does not let an older create's failure withdraw a newer create's placeholder", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    // Never answered, so the only thing that can touch the placeholder after
    // this point is the *older* create's failure. Letting the second create
    // succeed instead would republish its own placeholder over the top and the
    // assertion would hold whether the guard existed or not.
    const second = deferred<TmuxActionResult | undefined>();
    let seen = 0;
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind !== "createWindow") return { topologyGeneration: 3 };
      seen += 1;
      return seen === 1 ? first.promise : second.promise;
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createWindow("$1"));
    act(() => harness.navigation.createWindow("$1"));
    const newer = harness.setPendingTab.mock.calls.at(-1)?.[0];
    // The first request loses its race and answers second, with a refusal.
    first.resolve(undefined);
    await flush();
    await flush();

    expect(
      harness.setPendingTab.mock.calls.at(-1)?.[0],
      "the stale create cleared the placeholder the newer one had put up",
    ).toEqual(newer);
    await act(async () => renderer.unmount());
  });

  it("draws no placeholder for a new session until its ack names the workspace", async () => {
    const created = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind === "createSession") return created.promise;
      return { topologyGeneration: 3 };
    });
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work"));
    // A placeholder with no session is drawn nowhere: App only renders it into
    // the strip whose session it names, and before the ack there is none.
    expect(harness.setPendingTab.mock.calls[0]?.[0]?.sessionId).toBeUndefined();

    created.resolve({ sessionId: "$7", topologyGeneration: 2 });
    await flush();
    expect(harness.setPendingTab).toHaveBeenLastCalledWith(
      expect.objectContaining({ sessionId: "$7" }),
    );
    await act(async () => renderer.unmount());
  });

  it("sends the configured start directory with the create, and nothing when there is none", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(
      async () => ({ sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 2 }),
    );
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work", { directory: "~/dev" }));
    await flush();
    expect(performAction).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "createSession", name: "work", directory: "~/dev" }),
    );

    act(() => harness.navigation.createSession("other"));
    await flush();
    // Absent rather than empty: the wire mapping is what turns "no preference"
    // into the empty string the host reads, and it does that in one place.
    expect(performAction).toHaveBeenLastCalledWith({ kind: "createSession", name: "other" });
    await act(async () => renderer.unmount());
  });

  it("forwards the born-pinned flag with the create, and nothing when it is unset", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(
      async () => ({ sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 2 }),
    );
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work", { pinned: true }));
    await flush();
    expect(performAction).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "createSession", name: "work", pinned: true }),
    );

    act(() => harness.navigation.createSession("other", { pinned: false }));
    await flush();
    expect(performAction).toHaveBeenLastCalledWith({ kind: "createSession", name: "other" });
    await act(async () => renderer.unmount());
  });

  /**
   * The startup command's one delivery point.
   *
   * Everything that makes "exactly once, in the workspace that was created"
   * true is here: it fires on the ack's own pane id, it fires once, and it does
   * not fire for a create that failed, for an ack with no pane to type into, or
   * for an ack that came back after the app moved to another connection.
   */
  it("hands the acked pane identity to the create's callback exactly once", async () => {
    const onCreated = vi.fn();
    const harness = mountNavigation({
      performAction: vi.fn<ShellNavigationOptions["performAction"]>(
        async () => ({ sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 5 }),
      ),
    });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work", { onCreated }));
    await flush();
    await flush();

    expect(onCreated).toHaveBeenCalledOnce();
    expect(onCreated.mock.calls[0][0]).toEqual({
      sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 5,
    });
    expect(onCreated.mock.calls[0][1]).toMatchObject({ connectionKey: scope.connectionKey });
    await act(async () => renderer.unmount());
  });

  it("never runs the create's callback for a refusal, a throw, or an ack with no pane", async () => {
    for (const answer of [
      async () => undefined,
      async () => { throw new Error("host went away"); },
      async () => ({ sessionId: "$7", windowId: "@7", topologyGeneration: 5 }),
    ] satisfies (() => Promise<TmuxActionResult | undefined>)[]) {
      const onCreated = vi.fn();
      const harness = mountNavigation({
        performAction: vi.fn<ShellNavigationOptions["performAction"]>(answer),
      });
      const renderer = await harness.renderer();
      act(() => harness.navigation.createSession("work", { onCreated }));
      await flush();
      await flush();
      expect(onCreated).not.toHaveBeenCalled();
      await act(async () => renderer.unmount());
    }
  });

  /**
   * A refused create — the host judging the configured start directory is one
   * way to get one — leaves nothing behind: no placeholder in the strip, and no
   * workspace selected. The host's own message is what the user sees, and
   * `performAction` is what surfaces it.
   */
  it("leaves no placeholder and no selection behind when the host refuses the create", async () => {
    const harness = mountNavigation({
      performAction: vi.fn<ShellNavigationOptions["performAction"]>(async () => {
        throw new Error("workspace start directory /nope does not exist or is not a directory");
      }),
    });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work", { directory: "/nope" }));
    await flush();
    await flush();

    expect(harness.setPendingTab).toHaveBeenLastCalledWith(undefined);
    expect(harness.setActiveSessionId).not.toHaveBeenCalled();
    expect(harness.setActiveWindowId).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });

  it("refuses to deliver a create's callback across a connection change", async () => {
    // The race the ticket names: the ack arrives after the app has moved to a
    // different host, and the pane id it names belongs to a connection that is
    // no longer there. Typing into it is typing into someone else's workspace.
    const created = deferred<TmuxActionResult | undefined>();
    const onCreated = vi.fn();
    const harness = mountNavigation({
      performAction: vi.fn<ShellNavigationOptions["performAction"]>(async () => created.promise),
    });
    const renderer = await harness.renderer();

    act(() => harness.navigation.createSession("work", { onCreated }));
    await harness.rerender({ currentScope: { ...scope, connectionEpoch: scope.connectionEpoch + 1 } });
    created.resolve({ sessionId: "$7", windowId: "@7", paneId: "%7", topologyGeneration: 5 });
    await flush();
    await flush();

    expect(onCreated).not.toHaveBeenCalled();
    await act(async () => renderer.unmount());
  });
});

describe("optimistic terminal window switch", () => {
  /** The guard `useAppConnectionController` owns, as the hook sees it. */
  const guard = () => ({ current: undefined as OptimisticWindowSwitch | undefined });

  it("paints the switch before the host has answered", async () => {
    const selected = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => selected.promise);
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    expect(
      harness.setActiveWindowId,
      "the switch waited for the ack, which is the whole thing this removes",
    ).toHaveBeenCalledWith("@1");
    expect(optimisticWindow.current).toMatchObject({ sessionId: "$1", windowId: "@1" });

    selected.resolve({ topologyGeneration: 4 });
    await flush();
    await act(async () => renderer.unmount());
  });

  it("still sends the request it painted ahead of", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 4 }));
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    await flush();

    expect(
      performAction.mock.calls.some(([action]) => action.kind === "selectWindow" && action.windowId === "@1"),
      "an optimistic switch that never told tmux is a UI lying about where input goes",
    ).toBe(true);
    await act(async () => renderer.unmount());
  });

  it("holds the guard past the ack, until a snapshot can have caught up", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 7 }));
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    await flush();

    // Released by generation, not by the ack: the ack is not the snapshot.
    expect(optimisticWindow.current).toMatchObject({ windowId: "@1", throughGeneration: 7 });
    await act(async () => renderer.unmount());
  });

  it("rolls back to the host's window when the switch is refused", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => undefined);
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    await flush();

    // Actively, not by waiting for a snapshot: a refused switch changes
    // nothing on the host, so no further snapshot need arrive to correct it.
    expect(optimisticWindow.current, "a stuck guard leaves the shell ignoring tmux").toBeUndefined();
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@0");
    await act(async () => renderer.unmount());
  });

  it("rolls back when the switch throws", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => { throw new Error("gone"); });
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    await flush();

    expect(optimisticWindow.current).toBeUndefined();
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@0");
    await act(async () => renderer.unmount());
  });

  it("stays ack-gated where no guard is supplied", async () => {
    const selected = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => selected.promise);
    const harness = mountNavigation({ performAction });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    // Without somewhere to hold the switch against the next snapshot, painting
    // early would be reverted within one snapshot — worse than waiting.
    expect(harness.setActiveWindowId).not.toHaveBeenCalledWith("@1");

    selected.resolve({ topologyGeneration: 4 });
    await flush();
    expect(harness.setActiveWindowId).toHaveBeenCalledWith("@1");
    await act(async () => renderer.unmount());
  });

  it("does not arm the guard for a window that is already showing", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 4 }));
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@0"));
    await flush();

    expect(optimisticWindow.current, "nothing is outstanding, so nothing needs holding").toBeUndefined();
    await act(async () => renderer.unmount());
  });

  it("leaves the newest switch armed when two are made in a row", async () => {
    const first = deferred<TmuxActionResult | undefined>();
    let seen = 0;
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) => {
      if (action.kind !== "selectWindow") return { topologyGeneration: 3 };
      seen += 1;
      return seen === 1 ? first.promise : { topologyGeneration: 9 };
    });
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectWindow("@1"));
    act(() => harness.navigation.selectWindow("@0"));
    first.resolve({ topologyGeneration: 5 });
    await flush();
    await flush();

    // The stale answer must not stamp its generation onto the newer switch's
    // guard, which would release it early against a snapshot that predates it.
    expect(optimisticWindow.current?.windowId ?? "@0").toBe("@0");
    await act(async () => renderer.unmount());
  });
});

describe("optimistic workspace switch", () => {
  const guard = () => ({ current: undefined as OptimisticWindowSwitch | undefined });
  /** The same snapshot as `windows`, with a second workspace the host knows. */
  const acrossWorkspaces = [
    ...windows,
    { id: "@5", sessionId: "$2", index: 1, name: "five", active: false, layout: "" },
    { id: "@4", sessionId: "$2", index: 0, name: "four", active: true, layout: "" },
  ];

  it("paints the workspace and the window it shows in one commit, before the host answers", async () => {
    const selected = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => selected.promise);
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));

    expect(
      harness.setActiveSessionId,
      "the workspace waited for the ack, which is the whole thing this removes",
    ).toHaveBeenCalledWith("$2");
    // Both in the same handler: a frame with the new workspace and the old
    // workspace's window resolves to no active window at all.
    expect(harness.setActiveWindowId).toHaveBeenCalledWith("@4");
    expect(optimisticWindow.current).toMatchObject({ sessionId: "$2", windowId: "@4" });

    selected.resolve({ topologyGeneration: 4 });
    await flush();
    await act(async () => renderer.unmount());
  });

  it("still sends the select-session it painted ahead of", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 4 }));
    const harness = mountNavigation({ performAction, optimisticWindow: guard(), windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    await flush();

    expect(
      performAction.mock.calls.some(([action]) => action.kind === "selectSession" && action.sessionId === "$2"),
      "an optimistic switch that never told tmux is a UI lying about where input goes",
    ).toBe(true);
    await act(async () => renderer.unmount());
  });

  it("holds the guard past the ack, until a snapshot can have caught up", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 7 }));
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    await flush();

    // Released by generation, not by the ack: the ack is not the snapshot.
    expect(optimisticWindow.current).toMatchObject({ sessionId: "$2", windowId: "@4", throughGeneration: 7 });
    await act(async () => renderer.unmount());
  });

  it("rolls back to the workspace the host is still on when the switch is refused", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => undefined);
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    await flush();

    expect(optimisticWindow.current, "a stuck guard leaves the shell ignoring tmux").toBeUndefined();
    expect(harness.setActiveSessionId).toHaveBeenLastCalledWith("$1");
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@0");
    await act(async () => renderer.unmount());
  });

  it("rolls back when the switch throws", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => { throw new Error("gone"); });
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    await flush();

    expect(optimisticWindow.current).toBeUndefined();
    expect(harness.setActiveSessionId).toHaveBeenLastCalledWith("$1");
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@0");
    await act(async () => renderer.unmount());
  });

  it("commits a workspace whose windows this connection has never seen", async () => {
    const selected = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => selected.promise);
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));

    // Nothing to resolve to, so the workspace still moves and the controller's
    // effect names the window once its snapshot arrives.
    expect(harness.setActiveSessionId).toHaveBeenCalledWith("$2");
    expect(harness.setActiveWindowId).toHaveBeenCalledWith(undefined);
    expect(optimisticWindow.current).toMatchObject({ sessionId: "$2" });
    expect(optimisticWindow.current?.windowId).toBeUndefined();

    selected.resolve({ topologyGeneration: 4 });
    await flush();
    await act(async () => renderer.unmount());
  });

  it("stays ack-gated where no guard is supplied", async () => {
    const selected = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => selected.promise);
    const harness = mountNavigation({ performAction, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    expect(harness.setActiveSessionId).not.toHaveBeenCalledWith("$2");

    selected.resolve({ topologyGeneration: 4 });
    await flush();
    expect(harness.setActiveSessionId).toHaveBeenCalledWith("$2");
    // Ack-gated or not, the window lands with the workspace rather than a
    // paint later.
    expect(harness.setActiveWindowId).toHaveBeenCalledWith("@4");
    await act(async () => renderer.unmount());
  });

  it("does not arm the guard for the workspace already showing", async () => {
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async () => ({ topologyGeneration: 4 }));
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$1"));
    await flush();

    expect(optimisticWindow.current, "nothing is outstanding, so nothing needs holding").toBeUndefined();
    await act(async () => renderer.unmount());
  });

  it("does not let a refused workspace switch undo the window switch that superseded it", async () => {
    const workspace = deferred<TmuxActionResult | undefined>();
    const performAction = vi.fn<ShellNavigationOptions["performAction"]>(async (action) =>
      action.kind === "selectSession" && action.sessionId === "$2" ? workspace.promise : { topologyGeneration: 9 });
    const optimisticWindow = guard();
    const harness = mountNavigation({ performAction, optimisticWindow, windows: acrossWorkspaces });
    const renderer = await harness.renderer();

    act(() => harness.navigation.selectSession("$2"));
    act(() => harness.navigation.selectWindow("@1"));
    workspace.resolve(undefined);
    await flush();
    await flush();

    // The older workspace answer must not roll back a switch it no longer owns.
    expect(harness.setActiveWindowId).toHaveBeenLastCalledWith("@1");
    await act(async () => renderer.unmount());
  });
});
