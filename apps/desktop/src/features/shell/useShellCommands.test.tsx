// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import type { CommandId, CommandTarget } from "../../commands/registry";
import type { TerminalPaneController } from "../terminal/TerminalPane";
import type { TmuxActionResult } from "../tmux/actions";
import type { HostScopeToken } from "./hostScope";
import { defaultAppState, type PersistedAppState } from "./types";
import { resolveCommandTarget, useShellCommands } from "./useShellCommands";

const session: Session = { id: "$1", name: "muxflow", windowCount: 1, attachedClients: 1, order: 0 };
const window: TmuxWindow = { id: "@1", sessionId: "$1", index: 1, name: "zsh", active: true, layout: "" };
const pane: Pane = {
  id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true,
  width: 80, height: 24, left: 0, top: 0, currentPath: "/home/user", currentCommand: "zsh",
};
const snapshot: TmuxSnapshot = { sessions: [session], windows: [window], panes: [pane] };
const hostScope: HostScopeToken = {
  hostProfileId: "local", connectionKey: "local", connectionEpoch: 1,
  serverIdentity: "server-a", generation: 8,
};

const appTab: PersistedAppState["appTabs"][number] = {
  id: "tab-1", hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1",
  sessionName: "muxflow", kind: "file", resource: "/home/user/notes.md", title: "notes.md", order: 0,
};

const ambientSession: Session = { ...session, id: "$ambient", name: "ambient" };
const ambientWindow: TmuxWindow = { ...window, id: "@ambient", sessionId: ambientSession.id };
const ambientPane: Pane = { ...pane, id: "%ambient", sessionId: ambientSession.id, windowId: ambientWindow.id };
const ambientAppTab: PersistedAppState["appTabs"][number] = { ...appTab, id: "tab-ambient", sessionId: ambientSession.id };

type Options = Parameters<typeof useShellCommands>[0];

/**
 * Drives `runCommand` for one command, and reports everything the hook did.
 *
 * The hook is a `useCallback` over a wide options object, so a component is
 * the only honest way to call it; `create` is what the rest of this suite uses
 * for the same reason.
 */
async function run(
  commandId: CommandId,
  overrides: Partial<Options> & { result?: TmuxActionResult } = {},
  target?: CommandTarget,
) {
  // Typed against the real option, so a rename on `TmuxActionResult` breaks the
  // ⌘T test rather than leaving it green over a broken feature.
  const performAction = vi.fn<Options["performAction"]>(
    async () => overrides.result ?? { topologyGeneration: 8 },
  );
  const setConfirmation = vi.fn<Options["setConfirmation"]>();
  const setStatus = vi.fn<Options["setStatus"]>();
  const selectCreatedWindow = vi.fn<Options["selectCreatedWindow"]>();
  const setAppState = vi.fn();
  let call: ((commandId: CommandId, target?: CommandTarget) => Promise<void>) | undefined;

  function Harness() {
    const { runCommand } = useShellCommands({
      activePane: pane, activeSession: session, activeWindow: window,
      appState: { ...defaultAppState, appTabs: [appTab] },
      canMutate: true, combinedTabs: [], controllers: { current: new Map<string, TerminalPaneController>() },
      currentHostProfileId: "local", focusDirection: vi.fn(), generation: 8,
      hostScope, isHostScopeCurrent: () => true, jumpToUnreadAgent: vi.fn(),
      requestHostProfileDelete: vi.fn(), rowCommands: [], selectCreatedSession: vi.fn(),
      selectCreatedWindow, selectRelativeTab: vi.fn(), selectTabByIndex: vi.fn(),
      selectWorkspaceByIndex: vi.fn(), serverIdentity: "server-a", setAppState,
      setConfirmation, setPaletteOpen: vi.fn(), setSettingsOpen: vi.fn(),
      setShortcutEditorOpen: vi.fn(), setStatus, setTextPrompt: vi.fn(),
      setWorkspaceSwitcherOpen: vi.fn(), snapshot, stepFocusHistory: vi.fn(), windows: [window],
      ...overrides,
      performAction,
    });
    call = runCommand;
    return null;
  }

  let renderer!: ReturnType<typeof create>;
  await act(async () => { renderer = create(<Harness />); });
  await act(async () => { await call!(commandId, target); });
  await act(async () => renderer.unmount());
  return { performAction, setConfirmation, setStatus, selectCreatedWindow, setAppState };
}

describe("shell commands", () => {
  it.each([
    [undefined, "ambient", ["$ambient", "@ambient", "tab-ambient", "%ambient"]],
    [{ kind: "session", id: "$1" } as const, "session", ["$1", undefined, undefined, undefined]],
    [{ kind: "terminalTab", id: "@1" } as const, "terminalTab", [undefined, "@1", undefined, undefined]],
    [{ kind: "appTab", id: "tab-1" } as const, "appTab", [undefined, undefined, "tab-1", undefined]],
    [{ kind: "pane", id: "%1" } as const, "pane", [undefined, undefined, undefined, "%1"]],
  ])("resolves %s as an isolated %s command target", (target, kind, expected) => {
    const resolved = resolveCommandTarget({
      activePane: ambientPane,
      activeSession: ambientSession,
      activeWindow: ambientWindow,
      appState: { ...defaultAppState, appTabs: [appTab, ambientAppTab] },
      currentHostProfileId: "local",
      selectedAppTab: ambientAppTab,
      snapshot,
      windows: [ambientWindow],
    }, target);
    expect(resolved.kind).toBe(kind);
    expect([
      resolved.targetSession?.id,
      resolved.targetWindow?.id,
      resolved.targetAppTab?.id,
      resolved.targetPane?.id,
    ]).toEqual(expected);
  });

  it.each([
    { kind: "session", id: "$missing" } as const,
    { kind: "terminalTab", id: "@missing" } as const,
    { kind: "appTab", id: "tab-missing" } as const,
    { kind: "pane", id: "%missing" } as const,
  ])("never substitutes ambient state for stale explicit target $kind", (target) => {
    const resolved = resolveCommandTarget({
      activePane: ambientPane,
      activeSession: ambientSession,
      activeWindow: ambientWindow,
      appState: { ...defaultAppState, appTabs: [ambientAppTab] },
      currentHostProfileId: "local",
      selectedAppTab: ambientAppTab,
      snapshot,
      windows: [ambientWindow],
    }, target);
    expect([
      resolved.targetSession,
      resolved.targetWindow,
      resolved.targetAppTab,
      resolved.targetPane,
    ]).toEqual([undefined, undefined, undefined, undefined]);
  });

  it("selects the terminal tab ⌘T just created", async () => {
    // The host creates the window detached, so tmux's active window does not
    // move; the app mirrors that flag on every snapshot, so without an explicit
    // selection the new tab appears and the focus stays behind.
    const { performAction, selectCreatedWindow } = await run("window.new", {
      result: { windowId: "@9", topologyGeneration: 9 },
    });
    expect(performAction).toHaveBeenCalledWith({ kind: "createWindow", sessionId: "$1" });
    // The generation the *create* returned, not the one in scope: the create
    // moved the topology, and a selection sent against the older number is
    // rejected as stale and only lands on a retry.
    expect(selectCreatedWindow).toHaveBeenCalledWith("$1", "@9", 9);
  });

  it("does not steal focus into a window created on a host the user has left", async () => {
    const { selectCreatedWindow } = await run("window.new", {
      isHostScopeCurrent: () => false,
      result: { windowId: "@9", topologyGeneration: 9 },
    });
    expect(selectCreatedWindow).not.toHaveBeenCalled();
  });

  it("closes a terminal tab and a pane without a dialog, still telling the host it was confirmed", async () => {
    const closeWindow = await run("window.close", {}, { kind: "terminalTab", id: "@1" });
    expect(closeWindow.setConfirmation).not.toHaveBeenCalled();
    expect(closeWindow.performAction).toHaveBeenCalledWith(
      { kind: "closeWindow", sessionId: "$1", windowId: "@1", confirmed: true },
      { serverIdentity: "server-a", generation: 8 },
    );
    const closePane = await run("pane.close", {}, { kind: "pane", id: "%1" });
    expect(closePane.setConfirmation).not.toHaveBeenCalled();
    expect(closePane.performAction).toHaveBeenCalledWith(
      { kind: "closePane", sessionId: "$1", windowId: "@1", paneId: "%1", confirmed: true },
      { serverIdentity: "server-a", generation: 8 },
    );
  });

  it("still confirms closing a whole workspace", async () => {
    // A workspace takes every window in it. Different blast radius, and the
    // complaint that removed the other two dialogs was about tab close.
    const { setConfirmation, performAction } = await run("session.close", {}, { kind: "session", id: "$1" });
    expect(performAction).not.toHaveBeenCalled();
    expect(setConfirmation).toHaveBeenCalledTimes(1);
    expect(setConfirmation.mock.calls[0][0]).toMatchObject({
      commandId: "session.close",
      action: { kind: "closeSession", sessionId: "$1", confirmed: true },
      precondition: { serverIdentity: "server-a", generation: 8 },
    });
  });

  it("says nothing when closing a file tab, because the tab going is the message", async () => {
    const { setAppState, setStatus } = await run("window.close", {}, { kind: "appTab", id: "tab-1" });
    expect(setAppState).toHaveBeenCalledTimes(1);
    expect(setStatus).not.toHaveBeenCalled();
  });
});
