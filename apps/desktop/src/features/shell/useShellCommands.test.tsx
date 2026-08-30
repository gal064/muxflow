// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import type { CommandId, CommandTarget } from "../../commands/registry";
import type { TerminalPaneController } from "../terminal/TerminalPane";
import type { TmuxActionResult } from "../tmux/actions";
import { requestReconciledTmuxAction } from "../tmux/actionReconciliation";
import type { HostScopeToken } from "./hostScope";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { defaultAppState, type PersistedAppState } from "./types";
import { resolveCommandTarget, useShellCommands } from "./useShellCommands";
import { combineWorkspaceTabs } from "./model";
import type { AgentAttentionRollup } from "../agents/types";

/** An authority that positively covers these windows at the live generation. */
function agentAuthority(windowIds: readonly string[]) {
  const current = { hostProfileId: "local", serverIdentity: "server-a", connectionEpoch: 1, topologyGeneration: 8 };
  return { accepted: { ...current, coveredWindowIds: new Set(windowIds) }, current };
}

function agentInWindow(windowId: string): ReadonlyMap<string, AgentAttentionRollup> {
  return new Map([[windowId, {
    state: "working", adapterId: "codex", blocked: 0, working: 1, done: 0, unknown: 0, idle: 0, total: 1,
  } satisfies AgentAttentionRollup]]);
}

const session: Session = { id: "$1", name: "muxflow", windowCount: 1, attachedClients: 1, order: 0 };
const window: TmuxWindow = { id: "@1", sessionId: "$1", index: 1, name: "zsh", active: true, layout: "" };
const pane: Pane = {
  id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true,
  width: 80, height: 24, left: 0, top: 0, currentPath: "/home/operator", currentCommand: "zsh",
};
const snapshot: TmuxSnapshot = { sessions: [session], windows: [window], panes: [pane] };
const hostScope: HostScopeToken = {
  hostProfileId: "local", connectionKey: "local", connectionEpoch: 1,
  serverIdentity: "server-a", generation: 8,
};

const appTab: PersistedAppState["appTabs"][number] = {
  id: "tab-1", hostProfileId: "local", serverIdentity: "server-a", sessionId: "$1",
  sessionName: "muxflow", kind: "file", resource: "/home/operator/notes.md", title: "notes.md", order: 0,
};

const ambientSession: Session = { ...session, id: "$ambient", name: "ambient" };
const ambientWindow: TmuxWindow = { ...window, id: "@ambient", sessionId: ambientSession.id };
const ambientPane: Pane = { ...pane, id: "%ambient", sessionId: ambientSession.id, windowId: ambientWindow.id };
const ambientAppTab: PersistedAppState["appTabs"][number] = { ...appTab, id: "tab-ambient", sessionId: ambientSession.id };
type UnscopedCommandTarget = CommandTarget extends infer Target
  ? Target extends { scope: HostScopeToken } ? Omit<Target, "scope"> : never
  : never;
const target = (value: UnscopedCommandTarget): CommandTarget => ({ ...value, scope: hostScope } as CommandTarget);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  return { promise: new Promise<T>((done, fail) => { resolve = done; reject = fail; }), reject, resolve };
}

function resolvedIds(resolved: ReturnType<typeof resolveCommandTarget>) {
  switch (resolved.kind) {
    case "ambient": return [resolved.session?.id, resolved.window?.id, resolved.appTab?.id, resolved.pane?.id];
    case "session": return [resolved.value?.id, undefined, undefined, undefined];
    case "terminalTab": return [undefined, resolved.value?.id, undefined, undefined];
    case "appTab": return [undefined, undefined, resolved.value?.id, undefined];
    case "pane": return [undefined, undefined, undefined, resolved.value?.id];
    case "focusedSurface": return [undefined, resolved.window?.id, undefined, resolved.pane?.id];
  }
}

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
  const closeAppTab = vi.fn<Options["closeAppTab"]>();
  const createSession = vi.fn<Options["createSession"]>();
  const createWindow = vi.fn<Options["createWindow"]>();
  const setAppState = vi.fn();
  const setTextPrompt = vi.fn<Options["setTextPrompt"]>();
  let call: ((commandId: CommandId, target?: CommandTarget) => Promise<void>) | undefined;

  function Harness() {
    const { runCommand } = useShellCommands({
      activePane: pane, activeSession: session, activeWindow: window,
      appState: { ...defaultAppState, appTabs: [appTab] },
      canMutate: true, closeAppTab, combinedTabs: [], controllers: { current: new Map<string, TerminalPaneController>() },
      currentHostProfileId: "local", focusDirection: vi.fn(),
      hostScope, isHostScopeCurrent: () => true, jumpToUnreadAgent: vi.fn(),
      requestHostProfileDelete: vi.fn(), rowCommands: [], createSession,
      createWindow, selectRelativeTab: vi.fn(), selectTabByIndex: vi.fn(),
      selectWorkspaceByIndex: vi.fn(), serverIdentity: "server-a", setAppState,
      setConfirmation, setPaletteOpen: vi.fn(), setSettingsOpen: vi.fn(),
      setShortcutEditorOpen: vi.fn(), setStatus, setTextPrompt,
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
  return { closeAppTab, performAction, setConfirmation, setStatus, createSession, createWindow, setAppState, setTextPrompt };
}

describe("shell commands", () => {
  it("bookmarks mounted pane viewports before a rail command changes layout", async () => {
    for (const commandId of ["view.togglePanel", "view.showFiles", "view.showGit", "view.toggleSidebar"] as const) {
      const prepareForLayoutResize = vi.fn();
      const controller: TerminalPaneController = {
        focus: vi.fn(),
        prepareForLayoutResize,
        copy: vi.fn(async () => false),
        paste: vi.fn(async () => false),
        showSearch: vi.fn(),
        scrollToBottom: vi.fn(),
      };
      const result = await run(commandId, {
        controllers: { current: new Map([[pane.id, controller]]) },
      });
      expect(prepareForLayoutResize).toHaveBeenCalledOnce();
      expect(prepareForLayoutResize.mock.invocationCallOrder[0])
        .toBeLessThan(result.setAppState.mock.invocationCallOrder[0]);
    }
  });

  it("creates and commits a workspace with one tmux request", async () => {
    const result = await run("session.new");
    const prompt = result.setTextPrompt.mock.calls[0]?.[0];
    expect(prompt).not.toBeTypeOf("function");
    if (prompt && typeof prompt !== "function") {
      await act(async () => {
        prompt.submit("work");
        await Promise.resolve();
      });
    }
    expect(result.performAction).not.toHaveBeenCalled();
    expect(result.createSession).toHaveBeenCalledWith("work");
  });

  it("prefills a rename with the raw tmux name", async () => {
    // Status-glyph removal is presentation-only. The rename dialog exposes the
    // actual stored value rather than silently deleting its first character.
    const named = { ...window, name: "✳ Fix tests" };
    const result = await run("window.rename", {
      activeWindow: named,
      windows: [named],
      combinedTabs: combineWorkspaceTabs([named], [], agentInWindow("@1"), undefined, agentAuthority(["@1"])),
    });
    const prompt = result.setTextPrompt.mock.calls[0]?.[0];
    expect(prompt).toMatchObject({ initialValue: "✳ Fix tests" });
  });

  it("prefills a rename of an ordinary window with its exact tmux name", async () => {
    // ✓ and · are agent ticker frames *and* ordinary punctuation. The strip
    // hides a leading frame consistently, while rename still offers the exact
    // stored value.
    const named = { ...window, name: "✓ Deploy checklist" };
    const result = await run("window.rename", {
      activeWindow: named,
      windows: [named],
      combinedTabs: combineWorkspaceTabs([named], [], new Map(), undefined, agentAuthority(["@1"])),
    });
    expect(result.setTextPrompt.mock.calls[0]?.[0]).toMatchObject({ initialValue: "✓ Deploy checklist" });
  });

  it.each([
    [undefined, "ambient", ["$ambient", "@ambient", "tab-ambient", "%ambient"]],
    [target({ kind: "session", id: "$1" }), "session", ["$1", undefined, undefined, undefined]],
    [target({ kind: "terminalTab", id: "@1" }), "terminalTab", [undefined, "@1", undefined, undefined]],
    [target({ kind: "appTab", id: "tab-1" }), "appTab", [undefined, undefined, "tab-1", undefined]],
    [target({ kind: "pane", id: "%1" }), "pane", [undefined, undefined, undefined, "%1"]],
    [{ kind: "focusedSurface", paneId: "%1", scope: hostScope } as CommandTarget, "focusedSurface", [undefined, "@1", undefined, "%1"]],
  ])("resolves %s as an isolated %s command target", (target, kind, expected) => {
    const resolved = resolveCommandTarget({
      activePane: ambientPane,
      activeSession: ambientSession,
      activeWindow: ambientWindow,
      appState: { ...defaultAppState, appTabs: [appTab, ambientAppTab] },
      currentHostProfileId: "local",
      hostScope,
      selectedAppTab: ambientAppTab,
      snapshot,
      windows: [ambientWindow],
    }, target);
    expect(resolved.kind).toBe(kind);
    expect(resolvedIds(resolved)).toEqual(expected);
  });

  it.each([
    target({ kind: "session", id: "$missing" }),
    target({ kind: "terminalTab", id: "@missing" }),
    target({ kind: "appTab", id: "tab-missing" }),
    target({ kind: "pane", id: "%missing" }),
    { kind: "focusedSurface", paneId: "%missing", scope: hostScope } as CommandTarget,
  ])("never substitutes ambient state for stale explicit target $kind", (target) => {
    const resolved = resolveCommandTarget({
      activePane: ambientPane,
      activeSession: ambientSession,
      activeWindow: ambientWindow,
      appState: { ...defaultAppState, appTabs: [ambientAppTab] },
      currentHostProfileId: "local",
      hostScope,
      selectedAppTab: ambientAppTab,
      snapshot,
      windows: [ambientWindow],
    }, target);
    expect(resolvedIds(resolved)).toEqual([undefined, undefined, undefined, undefined]);
  });

  it("selects the terminal tab ⌘T just created", async () => {
    const { performAction, createWindow } = await run("window.new");
    expect(createWindow).toHaveBeenCalledWith("$1");
    expect(performAction).not.toHaveBeenCalled();
  });

  it.each([
    ["file", { ...appTab, kind: "file" as const }],
    ["markdown", { ...appTab, kind: "markdown" as const, viewMode: "split" as const }],
    ["gitDiff", { ...appTab, kind: "gitDiff" as const, resource: "unstaged:notes.md", gitRepositoryId: "repo", gitPath: "bm90ZXMubWQ=", gitTarget: "unstaged" as const }],
  ])("creates a terminal tab from a selected %s tab without closing the document", async (_kind, selectedAppTab) => {
    const { createWindow, closeAppTab, setAppState } = await run("window.new", {
      selectedAppTab, appState: { ...defaultAppState, appTabs: [selectedAppTab] },
    });
    expect(createWindow).toHaveBeenCalledExactlyOnceWith("$1");
    expect(closeAppTab).not.toHaveBeenCalled();
    expect(setAppState).not.toHaveBeenCalled();
  });

  it("closes a terminal tab and a pane without a dialog, still telling the host it was confirmed", async () => {
    // No dialog, so nothing pins the topology: the close guards the server it
    // was aimed at and reconciles against the generation live when it lands.
    const closeWindow = await run("window.close", {}, target({ kind: "terminalTab", id: "@1" }));
    expect(closeWindow.setConfirmation).not.toHaveBeenCalled();
    expect(closeWindow.performAction).toHaveBeenCalledWith(
      { kind: "closeWindow", sessionId: "$1", windowId: "@1", confirmed: true },
      { serverIdentity: "server-a", generation: 0 },
    );
    const closePane = await run("pane.close", {}, target({ kind: "pane", id: "%1" }));
    expect(closePane.setConfirmation).not.toHaveBeenCalled();
    expect(closePane.performAction).toHaveBeenCalledWith(
      { kind: "closePane", sessionId: "$1", windowId: "@1", paneId: "%1", confirmed: true },
      { serverIdentity: "server-a", generation: 0 },
    );
  });

  it("retries a terminal-tab close whose generation moved, instead of refusing it once", async () => {
    // Both halves of the fix in one place: what a close with no dialog stamps,
    // and what the reconciler then does with that stamp. An agent animating a
    // pane title moves the host's generation between the click and the request,
    // and the refusal that followed used to reach the user as an error they had
    // to click through.
    const { performAction } = await run("window.close", {}, target({ kind: "terminalTab", id: "@1" }));
    const [action, precondition] = performAction.mock.calls[0];
    const moved: HostScopeToken = { ...hostScope, generation: 9 };
    const request = vi.fn()
      .mockRejectedValueOnce(new Error("stale topology: generation changed"))
      .mockResolvedValueOnce({ topologyGeneration: 9 });

    await requestReconciledTmuxAction({
      clientId: "client",
      action,
      capturedPrecondition: precondition,
      initialScope: hostScope,
      currentScope: () => moved,
      request,
      waitForNewerScope: async () => moved,
    });

    expect(request).toHaveBeenCalledTimes(2);
  });

  it("routes ambient Close through the focused pane until it is the last pane", async () => {
    const secondPane: Pane = { ...pane, id: "%2", index: 1, active: false, left: 40, width: 40 };
    const split = await run("window.close", { snapshot: { ...snapshot, panes: [pane, secondPane] } });
    expect(split.performAction).toHaveBeenCalledWith(
      { kind: "closePane", sessionId: "$1", windowId: "@1", paneId: "%1", confirmed: true },
      { serverIdentity: "server-a", generation: 0 },
    );

    const last = await run("window.close");
    expect(last.performAction).toHaveBeenCalledWith(
      { kind: "closeWindow", sessionId: "$1", windowId: "@1", confirmed: true },
      { serverIdentity: "server-a", generation: 0 },
    );
  });

  it("rejects a menu-captured focused surface after its host scope is replaced", async () => {
    const secondPane: Pane = { ...pane, id: "%2", index: 1, active: false, left: 40, width: 40 };
    const stale: CommandTarget = { kind: "focusedSurface", paneId: "%1", scope: hostScope };
    const { performAction } = await run("window.close", {
      hostScope: { ...hostScope, connectionEpoch: 2, serverIdentity: "server-b" },
      snapshot: { ...snapshot, panes: [pane, secondPane] },
    }, stale);
    expect(performAction).not.toHaveBeenCalled();
  });

  it("sets the pinned-only filter from either of the two palette commands, without touching tmux", async () => {
    const on = await run("workspaces.showPinnedOnly", {});
    expect(on.performAction).not.toHaveBeenCalled();
    expect(on.setConfirmation).not.toHaveBeenCalled();
    expect(on.setAppState).toHaveBeenCalled();
    const applied = on.setAppState.mock.calls[0][0] as (state: PersistedAppState) => PersistedAppState;
    expect(applied(defaultAppState).shell.pinnedOnly).toBe(true);

    const off = await run("workspaces.showAll", {});
    const cleared = off.setAppState.mock.calls[0][0] as (state: PersistedAppState) => PersistedAppState;
    expect(cleared({ ...defaultAppState, shell: { ...defaultAppState.shell, pinnedOnly: true } }).shell.pinnedOnly).toBe(false);
  });

  it("still confirms closing a whole workspace, and pins no generation while the dialog stands", async () => {
    // A workspace takes every window in it. Different blast radius, and the
    // complaint that removed the other two dialogs was about tab close. The
    // dialog stays; the generation it used to pin does not. Consent is the
    // named target — this session id — and the dialog can stand for seconds
    // while an agent's animated pane title moves the host's generation, so a
    // pinned close was refused on arrival and, because a pin also disables the
    // retry ladder, refused again on the second click.
    const { setConfirmation, performAction } = await run("session.close", {}, target({ kind: "session", id: "$1" }));
    expect(performAction).not.toHaveBeenCalled();
    expect(setConfirmation).toHaveBeenCalledTimes(1);
    expect(setConfirmation.mock.calls[0][0]).toMatchObject({
      commandId: "session.close",
      action: { kind: "closeSession", sessionId: "$1", confirmed: true },
      precondition: { serverIdentity: "server-a", generation: 0 },
    });
  });

  it("says nothing when closing a file tab, because the tab going is the message", async () => {
    const { closeAppTab, setStatus } = await run("window.close", { selectedAppTab: appTab }, target({ kind: "appTab", id: "tab-1" }));
    expect(closeAppTab).toHaveBeenCalledWith(appTab, hostScope);
    expect(setStatus).not.toHaveBeenCalled();
  });

  it("drops a close whose durable host scope changes while editor flush is pending", async () => {
    const flush = deferred<void>();
    const unregister = editorFlushRegistry.register("delayed-close", () => flush.promise);
    let current = true;
    const closeAppTab = vi.fn<Options["closeAppTab"]>();
    const running = run("window.close", {
      closeAppTab,
      isHostScopeCurrent: () => current,
      selectedAppTab: appTab,
    }, target({ kind: "appTab", id: "tab-1" }));
    await Promise.resolve();
    current = false;
    flush.resolve();
    await running;
    unregister();
    expect(closeAppTab).not.toHaveBeenCalled();
  });

  it("does not publish an old editor flush failure after durable host replacement", async () => {
    const flush = deferred<void>();
    void flush.promise.catch(() => undefined);
    const unregister = editorFlushRegistry.register("delayed-failure", () => flush.promise);
    let current = true;
    const setStatus = vi.fn<Options["setStatus"]>();
    const running = run("window.close", {
      isHostScopeCurrent: () => current,
      selectedAppTab: appTab,
      setStatus,
    }, target({ kind: "appTab", id: "tab-1" }));
    await Promise.resolve();
    current = false;
    flush.reject(new Error("disk full"));
    await running;
    unregister();
    expect(setStatus).not.toHaveBeenCalled();
  });

  it.each([
    { hostProfileId: "replacement" },
    { connectionKey: "ssh:replacement" },
    { connectionEpoch: 2 },
    { serverIdentity: "server-b" },
  ])("rejects an explicit recycled tmux ID from replaced scope %o", async (replacement) => {
    const stale = target({ kind: "terminalTab", id: "@1" });
    const { performAction } = await run("window.close", {
      hostScope: { ...hostScope, ...replacement },
    }, stale);
    expect(performAction).not.toHaveBeenCalled();
  });
});
