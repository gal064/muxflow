import { useCallback, useMemo, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { HostProfile, Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import { createTmuxConfirmation, type PendingTmuxConfirmation } from "../../commands/destructiveConfirmation";
import type { PendingTextPrompt } from "../../commands/TextInputDialog";
import { commandRegistry, selectionIndex, type CommandContext, type CommandId, type CommandTarget } from "../../commands/registry";
import { rowCommandRegistry } from "../../commands/rowCommands";
import { nextSortMode } from "../agents/agentsList";
import type { TerminalPaneController } from "../terminal/TerminalPane";
import type { TmuxAction, TmuxActionResult } from "../tmux/actions";
import { relativeWindowReorderAction } from "../../app/windowSelection";
import { closeAppTab, reorderAppTab, type CombinedTab } from "./model";
import type { AppOwnedTab, PersistedAppState } from "./types";
import type { HostScopeToken } from "./hostScope";
import { editorFlushRegistry } from "../files/editorFlushRegistry";
import { shellAfterSidebarCommand } from "./responsiveShell";

type PerformAction = (
  action: TmuxAction,
  precondition?: { serverIdentity: string; generation: number },
) => Promise<TmuxActionResult | undefined>;

interface ShellCommandOptions {
  activePane?: Pane;
  activeSession?: Session;
  activeWindow?: TmuxWindow;
  appState: PersistedAppState;
  canMutate: boolean;
  combinedTabs: readonly CombinedTab[];
  /** The saved host Settings has picked, when it is one that can be deleted. */
  deletableHostProfile?: HostProfile;
  /** Asks for the destructive confirmation; App owns the dialog and the store call. */
  requestHostProfileDelete(profile: HostProfile): void;

  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  currentHostProfileId: string;
  focusDirection(direction: "left" | "right" | "up" | "down"): void;
  generation: number;
  hostScope: HostScopeToken;
  isHostScopeCurrent(scope: HostScopeToken): boolean;
  performAction: PerformAction;
  /** Live subscription to what the row surfaces currently offer. */
  rowCommands: readonly CommandId[];
  selectedAppTab?: AppOwnedTab;
  selectCreatedSession(sessionId: string): void;
  /**
   * Land on the terminal tab ⌘T just made. The host creates it detached, so
   * tmux's active window does not move and the app — which mirrors that flag
   * on every snapshot — would put the selection straight back.
   *
   * `generation` is the one the creation returned, not the one in scope: the
   * create bumped the topology and the app's own scope does not catch up until
   * the next snapshot, so a selection sent against the older number is
   * rejected as stale and only lands on a retry.
   */
  selectCreatedWindow(sessionId: string, windowId: string, generation: number): void;
  serverIdentity?: string;
  setAppState: Dispatch<SetStateAction<PersistedAppState>>;
  setConfirmation: Dispatch<SetStateAction<PendingTmuxConfirmation | undefined>>;
  setPaletteOpen: Dispatch<SetStateAction<boolean>>;
  setSettingsOpen: Dispatch<SetStateAction<boolean>>;
  setShortcutEditorOpen: Dispatch<SetStateAction<boolean>>;
  setStatus: Dispatch<SetStateAction<string>>;
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
  "activePane" | "activeSession" | "activeWindow" | "appState" | "currentHostProfileId" | "selectedAppTab" | "snapshot" | "windows"
>;

type ResolvedCommandTargetFields = {
  /** `ambient` is the only branch allowed to read the current shell selection. */
  targetSession?: Session;
  targetWindow?: TmuxWindow;
  targetAppTab?: AppOwnedTab;
  targetPane?: Pane;
  targetWindows: readonly TmuxWindow[];
};

export type ResolvedCommandTarget =
  | (ResolvedCommandTargetFields & { kind: "ambient" })
  | (ResolvedCommandTargetFields & { kind: "session" })
  | (ResolvedCommandTargetFields & { kind: "terminalTab" })
  | (ResolvedCommandTargetFields & { kind: "appTab" })
  | (ResolvedCommandTargetFields & { kind: "pane" });

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
      targetSession: inputs.activeSession,
      targetWindow: inputs.activeWindow,
      targetAppTab: inputs.selectedAppTab,
      targetPane: inputs.activePane,
      targetWindows: inputs.windows,
    };
  }
  switch (target.kind) {
    case "session":
      return {
        kind: target.kind,
        targetSession: inputs.snapshot.sessions.find((session) => session.id === target.id),
        targetWindows: [],
      };
    case "terminalTab": {
      const targetWindow = inputs.snapshot.windows.find((window) => window.id === target.id);
      return {
        kind: target.kind,
        targetWindow,
        targetWindows: targetWindow
          ? inputs.snapshot.windows.filter((window) => window.sessionId === targetWindow.sessionId).sort((left, right) => left.index - right.index)
          : [],
      };
    }
    case "appTab":
      return {
        kind: target.kind,
        targetAppTab: inputs.appState.appTabs.find((tab) => tab.id === target.id && tab.hostProfileId === inputs.currentHostProfileId),
        targetWindows: [],
      };
    case "pane":
      return {
        kind: target.kind,
        targetPane: inputs.snapshot.panes.find((pane) => pane.id === target.id),
        targetWindows: [],
      };
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
    const { targetSession, targetWindow, targetAppTab, targetPane, targetWindows } = resolveCommandTarget(options, target);
    if (commandId === "window.close" && targetAppTab) {
      try {
        await editorFlushRegistry.flushAll();
      } catch (error) {
        options.setStatus(`Could not close ${targetAppTab.title} because its editor did not save: ${String(error)}`);
        return;
      }
      // No toast: the tab is gone from the strip, which is the whole message.
      // Status is for what the user cannot see or must act on — the failure
      // branch above is exactly that, and stays.
      options.setAppState((current) => closeAppTab(current, options.currentHostProfileId, targetAppTab.id));
      return;
    }
    if (definition.destructive) {
      if (!options.serverIdentity) return;
      const close = closeTarget(commandId, { targetSession, targetWindow, targetPane });
      if (!close) return;
      // One dispatch, so `confirmed: true` and the authoritative precondition
      // are stamped in exactly one place whether or not a dialog is involved.
      // Whether a close *asks* is data — `confirmLabel` — not a second branch
      // of control flow above this one.
      const action: TmuxAction = { ...close.action, confirmed: true };
      const precondition = { serverIdentity: options.serverIdentity, generation: options.generation };
      if (close.confirmLabel) {
        options.setConfirmation(createTmuxConfirmation(commandId, definition.title, close.confirmLabel, action, precondition));
      } else {
        await options.performAction(action, precondition);
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
        const scope = options.hostScope;
        options.setTextPrompt({ title: "New workspace", label: "Workspace name", submit: (name) => {
          options.setTextPrompt(undefined);
          if (!options.isHostScopeCurrent(scope)) return options.setStatus("Workspace creation was cancelled because its host scope changed.");
          void options.performAction({ kind: "createSession", name }).then((result) => {
            if (result?.sessionId && options.isHostScopeCurrent(scope)) options.selectCreatedSession(result.sessionId);
          });
        } });
        return;
      }
      case "session.rename": {
        const scope = options.hostScope;
        if (targetSession) options.setTextPrompt({ title: "Rename workspace", label: "Workspace name", initialValue: targetSession.name, submit: (name) => {
          options.setTextPrompt(undefined);
          if (!options.isHostScopeCurrent(scope)) return options.setStatus("Workspace rename was cancelled because its host scope changed.");
          void options.performAction({ kind: "renameSession", sessionId: targetSession.id, name });
        } });
        return;
      }
      case "session.moveLeft": case "session.moveRight": {
        if (!targetSession) return;
        const ordered = [...options.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0));
        const current = ordered.findIndex((session) => session.id === targetSession.id);
        const index = current + (commandId === "session.moveLeft" ? -1 : 1);
        if (current >= 0 && index >= 0 && index < ordered.length) await options.performAction({ kind: "reorderSession", sessionId: targetSession.id, index });
        return;
      }
      case "window.new": {
        if (!targetSession) return;
        // Same shape as `session.new`: create, then select what came back, and
        // only if the app is still pointed at the host that created it.
        const scope = options.hostScope;
        const sessionId = targetSession.id;
        void options.performAction({ kind: "createWindow", sessionId }).then((result) => {
          if (result?.windowId && options.isHostScopeCurrent(scope)) {
            options.selectCreatedWindow(sessionId, result.windowId, result.topologyGeneration);
          }
        });
        return;
      }
      case "window.rename": {
        const scope = options.hostScope;
        if (targetWindow) options.setTextPrompt({ title: "Rename terminal tab", label: "Tab name", initialValue: targetWindow.name, submit: (name) => {
          options.setTextPrompt(undefined);
          if (!options.isHostScopeCurrent(scope)) return options.setStatus("Terminal-tab rename was cancelled because its host scope changed.");
          void options.performAction({ kind: "renameWindow", sessionId: targetWindow.sessionId, windowId: targetWindow.id, name });
        } });
        return;
      }
      case "window.moveLeft": case "window.moveRight": {
        if (targetAppTab) {
          const session = options.snapshot.sessions.find((item) => item.id === targetAppTab.sessionId);
          if (session) options.setAppState((current) => reorderAppTab(current, options.currentHostProfileId, options.serverIdentity, session, targetAppTab.id, commandId.endsWith("Left") ? "left" : "right"));
          return;
        }
        if (!targetWindow) return;
        const action = relativeWindowReorderAction(targetWindows, targetWindow.id, commandId.endsWith("Left") ? "left" : "right");
        if (action) await options.performAction(action);
        return;
      }
      case "pane.splitRight": case "pane.splitDown": if (targetPane) await options.performAction({ kind: commandId === "pane.splitRight" ? "splitPaneRight" : "splitPaneDown", sessionId: targetPane.sessionId, windowId: targetPane.windowId, paneId: targetPane.id, splitSize: 50 }); return;
      case "pane.focusLeft": options.focusDirection("left"); return;
      case "pane.focusRight": options.focusDirection("right"); return;
      case "pane.focusUp": options.focusDirection("up"); return;
      case "pane.focusDown": options.focusDirection("down"); return;
      case "pane.resizeLeft": case "pane.resizeRight": case "pane.resizeUp": case "pane.resizeDown": if (targetPane) await options.performAction({ kind: ({
        "pane.resizeLeft": "resizePaneLeft", "pane.resizeRight": "resizePaneRight",
        "pane.resizeUp": "resizePaneUp", "pane.resizeDown": "resizePaneDown",
      } as const)[commandId], paneId: targetPane.id, resizeCells: 2 }); return;
      case "pane.zoom": if (targetPane) await options.performAction({
        kind: "zoomPane",
        paneId: targetPane.id,
        windowId: targetPane.windowId,
        zoomed: !options.snapshot.windows.find((item) => item.id === targetPane.windowId)?.zoomed,
      }); return;
      case "terminal.copy": await options.controllers.current.get(targetPane?.id ?? "")?.copy(); return;
      case "terminal.paste": await options.controllers.current.get(targetPane?.id ?? "")?.paste(); return;
      case "terminal.search": options.controllers.current.get(targetPane?.id ?? "")?.showSearch(); return;
      case "terminal.scrollBottom": options.controllers.current.get(targetPane?.id ?? "")?.scrollToBottom(); return;
    }
  }, [options]);

  const commandContext: CommandContext = useMemo(() => ({
    canMutate: options.canMutate,
    hasSession: Boolean(options.activeSession),
    hasWindow: Boolean(options.activeWindow && !options.selectedAppTab),
    hasPane: Boolean(options.activePane && !options.selectedAppTab),
    hasTab: Boolean(options.activeWindow || options.selectedAppTab),
    canMoveSessionUp: Boolean(options.activeSession && [...options.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0)).findIndex((session) => session.id === options.activeSession!.id) > 0),
    canMoveSessionDown: Boolean(options.activeSession && [...options.snapshot.sessions].sort((left, right) => (left.order ?? 0) - (right.order ?? 0)).findIndex((session) => session.id === options.activeSession!.id) < options.snapshot.sessions.length - 1),
    canMoveTabLeft: options.selectedAppTab
      ? Boolean(options.combinedTabs.find((tab) => tab.key === `app:${options.selectedAppTab!.id}`)?.canMoveLeft)
      : Boolean(options.activeWindow && relativeWindowReorderAction(options.windows, options.activeWindow.id, "left")),
    canMoveTabRight: options.selectedAppTab
      ? Boolean(options.combinedTabs.find((tab) => tab.key === `app:${options.selectedAppTab!.id}`)?.canMoveRight)
      : Boolean(options.activeWindow && relativeWindowReorderAction(options.windows, options.activeWindow.id, "right")),
    hasHostProfile: Boolean(options.deletableHostProfile),
    rowCommands: options.rowCommands,
    run: runCommand,
  }), [options, runCommand]);

  return { commandContext, runCommand };
}
