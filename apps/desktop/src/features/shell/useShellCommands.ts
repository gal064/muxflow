import { useCallback, useMemo, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { HostProfile, Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import { createTmuxConfirmation, type PendingTmuxConfirmation } from "../../commands/destructiveConfirmation";
import type { PendingTextPrompt } from "../../commands/TextInputDialog";
import { commandRegistry, selectionIndex, type CommandContext, type CommandId, type CommandTarget } from "../../commands/registry";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { nextSortMode } from "../agents/agentsList";
import type { TerminalPaneController } from "../terminal/TerminalPane";
import { closePrecondition, type TmuxAction, type TmuxActionResult } from "../tmux/actions";
import { relativeWindowReorderAction } from "../../app/windowSelection";
import { reorderAppTab, type CombinedTab } from "./model";
import type { AppOwnedTab, PersistedAppState } from "./types";

/**
 * The app tab behind a selection, when it is one that can be reordered.
 *
 * The lookup is by key, so a placeholder can never match it; the narrowing is
 * what lets the reorder flags be read without the union claiming a pending tab
 * might turn up here.
 */
function movableAppTab(tabs: readonly CombinedTab[], appTabId: string) {
  const found = tabs.find((tab) => tab.key === `app:${appTabId}`);
  return found?.kind === "app" ? found : undefined;
}

import { sameHostConnection, type HostScopeToken } from "./hostScope";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { shellAfterSidebarCommand } from "./responsiveShell";

type PerformAction = (
  action: TmuxAction,
  precondition?: { serverIdentity: string; generation: number },
) => Promise<TmuxActionResult | undefined>;

/**
 * The host a targeted command runs on: what it knows, how it is identified,
 * and the action path that reaches it. The active host is one of these; a
 * host shown beside it is another, with its own snapshot and its own client.
 */
export interface CommandHost {
  scope: HostScopeToken;
  snapshot: TmuxSnapshot;
  serverIdentity?: string;
  performAction: PerformAction;
  isScopeCurrent(scope: HostScopeToken): boolean;
}

interface ShellCommandOptions {
  activePane?: Pane;
  activeSession?: Session;
  activeWindow?: TmuxWindow;
  appState: PersistedAppState;
  canMutate: boolean;
  canCreateWorkspace: boolean;
  closeAppTab(tab: AppOwnedTab, scope: HostScopeToken): void;
  combinedTabs: readonly CombinedTab[];
  /** The saved host Settings has picked, when it is one that can be deleted. */
  deletableHostProfile?: HostProfile;
  /** Asks for the destructive confirmation; App owns the dialog and the store call. */
  requestHostProfileDelete(profile: HostProfile): void;

  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  currentHostProfileId: string;
  focusDirection(direction: "left" | "right" | "up" | "down"): void;
  hostScope: HostScopeToken;
  isHostScopeCurrent(scope: HostScopeToken): boolean;
  /**
   * The host a target's scope names — the active one or a peer — while that
   * scope is still the host's live connection. A stale scope, or a host no
   * longer shown, resolves to nothing, and the command finds no subject.
   */
  hostForScope(scope: HostScopeToken): CommandHost | undefined;
  performAction: PerformAction;
  /** Live subscription to what the row surfaces currently offer. */
  rowCommands: readonly CommandId[];
  selectedAppTab?: AppOwnedTab;
  requestNewWorkspace(): void;
  createWindow(sessionId: string): void;
  serverIdentity?: string;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  setConfirmation: Dispatch<SetStateAction<PendingTmuxConfirmation | undefined>>;
  setPaletteOpen: Dispatch<SetStateAction<boolean>>;
  setSettingsOpen: Dispatch<SetStateAction<boolean>>;
  setShortcutEditorOpen: Dispatch<SetStateAction<boolean>>;
  setStatus: (status: string) => void;
  setTextPrompt: Dispatch<SetStateAction<PendingTextPrompt | undefined>>;
  setWorkspaceSwitcherOpen: Dispatch<SetStateAction<boolean>>;
  snapshot: TmuxSnapshot;
  windows: readonly TmuxWindow[];
  /** ⌘1–9: the nth workspace in the sidebar's order. */
  selectWorkspaceByIndex(index: number): void;
  /** ⌃1–9 and ⌘⇧[/]: positions in the one combined tab strip. */
  selectTabByIndex(index: number): void;
  selectRelativeTab(direction: -1 | 1): void;
  /** ⌘⇧U. */
  jumpToUnreadAgent(): void;
  stepFocusHistory(direction: "back" | "forward"): void;
}

type CommandTargetInputs = Pick<ShellCommandOptions,
  "activePane" | "activeSession" | "activeWindow" | "appState" | "currentHostProfileId" | "hostScope" | "selectedAppTab" | "snapshot" | "windows"
>;

export type ResolvedCommandTarget =
  | {
    kind: "ambient";
    appTab?: AppOwnedTab;
    pane?: Pane;
    session?: Session;
    window?: TmuxWindow;
    windows: readonly TmuxWindow[];
  }
  | { kind: "session"; value?: Session }
  | { kind: "terminalTab"; value?: TmuxWindow; windows: readonly TmuxWindow[] }
  | { kind: "appTab"; value?: AppOwnedTab }
  | { kind: "pane"; value?: Pane }
  | { kind: "focusedSurface"; pane?: Pane; window?: TmuxWindow };

/**
 * Resolve a command subject without mixing an explicit surface with whatever
 * happens to be selected behind it. Context menus and tab close buttons pass a
 * target; a missing/stale explicit ID therefore resolves to no subject instead
 * of falling through to the active workspace, tab, app tab, or pane.
 */
export function resolveCommandTarget(
  inputs: CommandTargetInputs,
  target?: CommandTarget,
): ResolvedCommandTarget {
  if (!target) {
    return {
      kind: "ambient",
      session: inputs.activeSession,
      window: inputs.activeWindow,
      appTab: inputs.selectedAppTab,
      pane: inputs.activePane,
      windows: inputs.windows,
    };
  }
  const scopeCurrent = sameHostConnection(target.scope, inputs.hostScope);
  switch (target.kind) {
    case "session":
      return {
        kind: target.kind,
        value: scopeCurrent ? inputs.snapshot.sessions.find((session) => session.id === target.id) : undefined,
      };
    case "terminalTab": {
      const targetWindow = scopeCurrent
        ? inputs.snapshot.windows.find((window) => window.id === target.id)
        : undefined;
      return {
        kind: target.kind,
        value: targetWindow,
        windows: targetWindow
          ? inputs.snapshot.windows.filter((window) => window.sessionId === targetWindow.sessionId).sort((left, right) => left.index - right.index)
          : [],
      };
    }
    case "appTab":
      return {
        kind: target.kind,
        value: scopeCurrent ? inputs.appState.appTabs.find((tab) => tab.id === target.id
          && tab.hostProfileId === inputs.currentHostProfileId
          && tab.serverIdentity === target.scope.serverIdentity) : undefined,
      };
    case "pane":
      return {
        kind: target.kind,
        value: scopeCurrent ? inputs.snapshot.panes.find((pane) => pane.id === target.id) : undefined,
      };
    case "focusedSurface": {
      const pane = scopeCurrent
        ? inputs.snapshot.panes.find((candidate) => candidate.id === target.paneId)
        : undefined;
      return {
        kind: target.kind,
        pane,
        window: pane ? inputs.snapshot.windows.find((candidate) => candidate.id === pane.windowId) : undefined,
      };
    }
  }
}

/**
 * What a destructive command closes, and whether it asks first.
 *
 * `confirmLabel` present means a dialog names that target; absent means the
 * close happens on the spot. A terminal tab and a pane are the surface the
 * user is looking at and their disappearance is the confirmation — the same
 * call this user's own terminal makes with `confirm-close-surface = false`. A
 * workspace takes every window in it, which is a different blast radius.
 *
 * Either way the action reaches the host identically: `confirmed: true` plus
 * the precondition captured when the command ran. Only the gate differs.
 */
function closeTarget(
  commandId: CommandId,
  targets: { targetSession?: Session; targetWindow?: TmuxWindow; targetPane?: Pane },
): { action: TmuxAction; confirmLabel?: string } | undefined {
  const { targetSession, targetWindow, targetPane } = targets;
  if (commandId === "session.close" && targetSession) {
    return {
      action: { kind: "closeSession", sessionId: targetSession.id },
      confirmLabel: `workspace “${targetSession.name}” and all of its windows`,
    };
  }
  if (commandId === "window.close" && targetWindow) {
    return { action: { kind: "closeWindow", sessionId: targetWindow.sessionId, windowId: targetWindow.id } };
  }
  if (commandId === "pane.close" && targetPane) {
    return { action: { kind: "closePane", sessionId: targetPane.sessionId, windowId: targetPane.windowId, paneId: targetPane.id } };
  }
  return undefined;
}

export function useShellCommands(options: ShellCommandOptions): {
  commandContext: CommandContext;
  runCommand(commandId: CommandId, target?: CommandTarget): Promise<void>;
} {
  const runCommand = useCallback(async (commandId: CommandId, target?: CommandTarget) => {
    const definition = commandRegistry.find((command) => command.id === commandId);
    if (!definition) return;
    // Row commands belong to the surface that published them; this hook has no
    // business knowing what "the selected file" is. The row may also have gone
    // between the palette opening and Enter — a panel closed, a connection
    // dropped — and that has to be said rather than silently doing nothing.
    if (definition.requires === "row") {
      const outcome = rowCommandRegistry.run(commandId);
      if (!outcome.ran) options.setStatus(`${definition.title.replace(/…$/, "")} is unavailable: nothing is selected in that panel any more.`);
      return;
    }
    // Ambient commands act on the host on screen; a targeted one acts on the
    // host its row came from, which may be shown beside the active one. A
    // target whose host cannot be found resolves against the active host and
    // fails its scope check there, so it finds no subject.
    const activeHost: CommandHost = {
      scope: options.hostScope, snapshot: options.snapshot, serverIdentity: options.serverIdentity,
      performAction: options.performAction, isScopeCurrent: options.isHostScopeCurrent,
    };
    const host = (target && options.hostForScope(target.scope)) ?? activeHost;
    const resolvedTarget = resolveCommandTarget({ ...options, hostScope: host.scope, snapshot: host.snapshot }, target);
    const targetSession = resolvedTarget.kind === "session" ? resolvedTarget.value
      : resolvedTarget.kind === "ambient" ? resolvedTarget.session : undefined;
    const targetWindow = resolvedTarget.kind === "terminalTab" ? resolvedTarget.value
      : resolvedTarget.kind === "focusedSurface" ? resolvedTarget.window
      : resolvedTarget.kind === "ambient" ? resolvedTarget.window : undefined;
    const targetAppTab = resolvedTarget.kind === "appTab" ? resolvedTarget.value
      : resolvedTarget.kind === "ambient" ? resolvedTarget.appTab : undefined;
    const targetPane = resolvedTarget.kind === "pane" ? resolvedTarget.value
      : resolvedTarget.kind === "focusedSurface" ? resolvedTarget.pane
      : resolvedTarget.kind === "ambient" ? resolvedTarget.pane : undefined;
    const targetWindows = resolvedTarget.kind === "terminalTab" || resolvedTarget.kind === "ambient"
      ? resolvedTarget.windows : [];
    if (commandId === "window.close" && targetAppTab) {
      const closeScope = target?.scope ?? host.scope;
      try {
        await editorFlushRegistry.flushAll();
      } catch (error) {
        if (host.isScopeCurrent(closeScope)) {
          options.setStatus(`Could not close ${targetAppTab.title} because its editor did not save: ${String(error)}`);
        }
        return;
      }
      // No toast: the tab is gone from the strip, which is the whole message.
      // Status is for what the user cannot see or must act on — the failure
      // branch above is exactly that, and stays.
      if (!host.isScopeCurrent(closeScope)) return;
      options.closeAppTab(targetAppTab, closeScope);
      return;
    }
    if (definition.destructive) {
      if (!host.serverIdentity) return;
      // The ambient Close command (keyboard, palette or application menu)
      // closes the focused pane while a terminal tab is split. An explicit tab
      // target is the tab context menu's "Close tab" and remains whole-tab.
      const paneFirst = commandId === "window.close"
        && (resolvedTarget.kind === "ambient" || resolvedTarget.kind === "focusedSurface")
        && targetWindow
        && targetPane
        && host.snapshot.panes.filter((pane) => pane.windowId === targetWindow.id).length > 1;
      const close = closeTarget(paneFirst ? "pane.close" : commandId, { targetSession, targetWindow, targetPane });
      if (!close) return;
      // One dispatch, so `confirmed: true` and the authoritative precondition
      // are stamped in exactly one place whether or not a dialog is involved.
      // Whether a close *asks* is data — `confirmLabel` — not a second branch
      // of control flow above this one.
      const action: TmuxAction = { ...close.action, confirmed: true };
      const precondition = closePrecondition(host.serverIdentity);
      if (close.confirmLabel) {
        options.setConfirmation(createTmuxConfirmation(commandId, definition.title, close.confirmLabel, action, precondition, host.scope));
      } else {
        await host.performAction(action, precondition);
      }
      return;
    }
    const workspaceIndex = selectionIndex(commandId, "workspace.select");
    if (workspaceIndex !== undefined) return options.selectWorkspaceByIndex(workspaceIndex - 1);
    const tabIndex = selectionIndex(commandId, "tab.select");
    if (tabIndex !== undefined) return options.selectTabByIndex(tabIndex - 1);

    switch (commandId) {
      case "commands.show": options.setPaletteOpen(true); return;
      case "workspaces.switch": options.setWorkspaceSwitcherOpen(true); return;
      case "shortcuts.configure": options.setShortcutEditorOpen(true); return;
      case "settings.show": options.setSettingsOpen(true); return;
      // The confirmation is the host picker's own, not the tmux one the
      // `destructive` flag routes to — a saved host is a local preference, and
      // there is no server identity or topology generation to capture.
      case "host.delete": if (options.deletableHostProfile) options.requestHostProfileDelete(options.deletableHostProfile); return;
      case "view.toggleSidebar":
      case "view.togglePanel":
      case "view.showFiles":
      case "view.showGit":
        options.setAppState((current) => ({
          ...current,
          shell: shellAfterSidebarCommand(current.shell, commandId),
        }));
        return;
      case "agents.toggleSort":
        options.setAppState((current) => ({ ...current, shell: { ...current.shell, agentSort: nextSortMode(current.shell.agentSort) } }));
        return;
      case "agents.jumpUnread": options.jumpToUnreadAgent(); return;
      case "focus.workspaces": document.querySelector<HTMLButtonElement>(".workspace-button[aria-current=true], .workspace-button")?.focus(); return;
      case "focus.tabs": document.querySelector<HTMLButtonElement>(".tab-select[aria-selected=true], .tab-select")?.focus(); return;
      case "focus.back": options.stepFocusHistory("back"); return;
      case "focus.forward": options.stepFocusHistory("forward"); return;
      case "tab.previous": options.selectRelativeTab(-1); return;
      case "tab.next": options.selectRelativeTab(1); return;
      case "session.new": {
        options.requestNewWorkspace();
        return;
      }
      case "session.rename": {
        const scope = host.scope;
        if (targetSession) options.setTextPrompt({ title: "Rename workspace", label: "Workspace name", initialValue: targetSession.name, submit: (name) => {
          options.setTextPrompt(undefined);
          if (!host.isScopeCurrent(scope)) return options.setStatus("Workspace rename was cancelled because its host scope changed.");
          void host.performAction({ kind: "renameSession", sessionId: targetSession.id, name });
        } });
        return;
      }
      case "session.moveLeft": case "session.moveRight": {
        if (!targetSession) return;
        const ordered = [...host.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0));
        const current = ordered.findIndex((session) => session.id === targetSession.id);
        const index = current + (commandId === "session.moveLeft" ? -1 : 1);
        if (current >= 0 && index >= 0 && index < ordered.length) await host.performAction({ kind: "reorderSession", sessionId: targetSession.id, index });
        return;
      }
      case "workspaces.showPinnedOnly": case "workspaces.showAll":
        options.setAppState((current) => ({
          ...current,
          shell: { ...current.shell, pinnedOnly: commandId === "workspaces.showPinnedOnly" },
        }));
        return;
      case "window.new": {
        if (!targetSession) return;
        options.createWindow(targetSession.id);
        return;
      }
      case "window.rename": {
        const scope = host.scope;
        // Display-only normalization must not rewrite the stored tmux name.
        if (targetWindow) options.setTextPrompt({ title: "Rename terminal tab", label: "Tab name", initialValue: targetWindow.name, submit: (name) => {
          options.setTextPrompt(undefined);
          if (!host.isScopeCurrent(scope)) return options.setStatus("Terminal-tab rename was cancelled because its host scope changed.");
          void host.performAction({ kind: "renameWindow", sessionId: targetWindow.sessionId, windowId: targetWindow.id, name });
        } });
        return;
      }
      case "window.moveLeft": case "window.moveRight": {
        if (targetAppTab) {
          const session = host.snapshot.sessions.find((item) => item.id === targetAppTab.sessionId);
          if (session) options.setAppState((current) => reorderAppTab(current, options.currentHostProfileId, options.serverIdentity, session, targetAppTab.id, commandId.endsWith("Left") ? "left" : "right"));
          return;
        }
        if (!targetWindow) return;
        const action = relativeWindowReorderAction(targetWindows, targetWindow.id, commandId.endsWith("Left") ? "left" : "right");
        if (action) await host.performAction(action);
        return;
      }
      case "pane.splitRight": case "pane.splitDown": if (targetPane) await host.performAction({ kind: commandId === "pane.splitRight" ? "splitPaneRight" : "splitPaneDown", sessionId: targetPane.sessionId, windowId: targetPane.windowId, paneId: targetPane.id, splitSize: 50 }); return;
      case "pane.focusLeft": options.focusDirection("left"); return;
      case "pane.focusRight": options.focusDirection("right"); return;
      case "pane.focusUp": options.focusDirection("up"); return;
      case "pane.focusDown": options.focusDirection("down"); return;
      case "pane.resizeLeft": case "pane.resizeRight": case "pane.resizeUp": case "pane.resizeDown": if (targetPane) await host.performAction({ kind: ({
        "pane.resizeLeft": "resizePaneLeft", "pane.resizeRight": "resizePaneRight",
        "pane.resizeUp": "resizePaneUp", "pane.resizeDown": "resizePaneDown",
      } as const)[commandId], paneId: targetPane.id, resizeCells: 2 }); return;
      case "pane.zoom": if (targetPane) await host.performAction({
        kind: "zoomPane",
        paneId: targetPane.id,
        windowId: targetPane.windowId,
        zoomed: !host.snapshot.windows.find((item) => item.id === targetPane.windowId)?.zoomed,
      }); return;
      case "terminal.copy": await options.controllers.current.get(targetPane?.id ?? "")?.copy(); return;
      case "terminal.paste": await options.controllers.current.get(targetPane?.id ?? "")?.paste(); return;
      case "terminal.search": options.controllers.current.get(targetPane?.id ?? "")?.showSearch(); return;
      case "terminal.scrollBottom": options.controllers.current.get(targetPane?.id ?? "")?.scrollToBottom(); return;
    }
  }, [options]);

  const commandContext: CommandContext = useMemo(() => ({
    canMutate: options.canMutate,
    canCreateWorkspace: options.canCreateWorkspace,
    hasSession: Boolean(options.activeSession),
    hasWindow: Boolean(options.activeWindow && !options.selectedAppTab),
    hasPane: Boolean(options.activePane && !options.selectedAppTab),
    hasTab: Boolean(options.activeWindow || options.selectedAppTab),
    canMoveSessionUp: Boolean(options.activeSession && [...options.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0)).findIndex((session) => session.id === options.activeSession!.id) > 0),
    canMoveSessionDown: Boolean(options.activeSession && [...options.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0)).findIndex((session) => session.id === options.activeSession!.id) < options.snapshot.sessions.length - 1),
    canMoveTabLeft: options.selectedAppTab
      ? Boolean(movableAppTab(options.combinedTabs, options.selectedAppTab.id)?.canMoveLeft)
      : Boolean(options.activeWindow && relativeWindowReorderAction(options.windows, options.activeWindow.id, "left")),
    canMoveTabRight: options.selectedAppTab
      ? Boolean(movableAppTab(options.combinedTabs, options.selectedAppTab.id)?.canMoveRight)
      : Boolean(options.activeWindow && relativeWindowReorderAction(options.windows, options.activeWindow.id, "right")),
    hasHostProfile: Boolean(options.deletableHostProfile),
    pinnedOnly: options.appState.shell.pinnedOnly,
    rowCommands: options.rowCommands,
    run: runCommand,
  }), [options, runCommand]);

  return { commandContext, runCommand };
}
