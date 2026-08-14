import { useCallback, useMemo, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "../../app/types";
import { createTmuxConfirmation, type PendingTmuxConfirmation } from "../../commands/destructiveConfirmation";
import type { PendingTextPrompt } from "../../commands/TextInputDialog";
import { commandRegistry, selectionIndex, type CommandContext, type CommandId, type CommandTarget } from "../../commands/registry";
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
  compactViewport: boolean;
  controllers: MutableRefObject<Map<string, TerminalPaneController>>;
  currentHostProfileId: string;
  focusDirection(direction: "left" | "right" | "up" | "down"): void;
  generation: number;
  hostScope: HostScopeToken;
  isHostScopeCurrent(scope: HostScopeToken): boolean;
  performAction: PerformAction;
  selectedAppTab?: AppOwnedTab;
  selectCreatedSession(sessionId: string): void;
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

export function useShellCommands(options: ShellCommandOptions): {
  commandContext: CommandContext;
  runCommand(commandId: CommandId, target?: CommandTarget): Promise<void>;
} {
  const runCommand = useCallback(async (commandId: CommandId, target?: CommandTarget) => {
    const definition = commandRegistry.find((command) => command.id === commandId);
    if (!definition) return;
    const targetSession = target?.kind === "session"
      ? options.snapshot.sessions.find((session) => session.id === target.id)
      : options.activeSession;
    const targetWindow = target?.kind === "terminalTab"
      ? options.snapshot.windows.find((window) => window.id === target.id)
      : options.activeWindow;
    const targetAppTab = target?.kind === "appTab"
      ? options.appState.appTabs.find((tab) => tab.id === target.id && tab.hostProfileId === options.currentHostProfileId)
      : options.selectedAppTab;
    const targetPane = target?.kind === "pane"
      ? options.snapshot.panes.find((pane) => pane.id === target.id)
      : options.activePane;
    const targetWindows = targetWindow
      ? options.snapshot.windows.filter((window) => window.sessionId === targetWindow.sessionId).sort((left, right) => left.index - right.index)
      : options.windows;
    if (commandId === "window.close" && targetAppTab) {
      try {
        await editorFlushRegistry.flushAll();
      } catch (error) {
        options.setStatus(`Could not close ${targetAppTab.title} because its editor did not save: ${String(error)}`);
        return;
      }
      options.setAppState((current) => closeAppTab(current, options.currentHostProfileId, targetAppTab.id));
      options.setStatus(`Closed ${targetAppTab.title}`);
      return;
    }
    if (commandId === "session.close" && targetSession) {
      if (!options.serverIdentity) return;
      options.setConfirmation(createTmuxConfirmation(
        commandId,
        definition.title,
        `workspace “${targetSession.name}” and all of its windows`,
        { kind: "closeSession", sessionId: targetSession.id },
        { serverIdentity: options.serverIdentity, generation: options.generation },
      ));
      return;
    }
    if (definition.destructive) {
      if (!options.serverIdentity) return;
      const captured = commandId === "window.close" && targetWindow
        ? {
            label: `terminal tab “${targetWindow.name}” and all of its panes`,
            action: { kind: "closeWindow", sessionId: targetWindow.sessionId, windowId: targetWindow.id } as TmuxAction,
          }
        : commandId === "pane.close" && targetPane
            ? {
                label: `pane ${targetPane.id}`,
                action: { kind: "closePane", sessionId: targetPane.sessionId, windowId: targetPane.windowId, paneId: targetPane.id } as TmuxAction,
              }
            : undefined;
      if (!captured) return;
      options.setConfirmation(createTmuxConfirmation(
        commandId, definition.title, captured.label, captured.action,
        { serverIdentity: options.serverIdentity, generation: options.generation },
      ));
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
      case "view.toggleSidebar":
      case "view.togglePanel":
      case "view.showFiles":
      case "view.showGit":
        options.setAppState((current) => ({
          ...current,
          shell: shellAfterSidebarCommand(current.shell, commandId, options.compactViewport),
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
      case "window.new": if (targetSession) await options.performAction({ kind: "createWindow", sessionId: targetSession.id }); return;
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
      case "pane.zoom": if (targetPane) await options.performAction({ kind: "zoomPane", paneId: targetPane.id, windowId: targetPane.windowId, zoomed: !targetWindow?.zoomed }); return;
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
    run: runCommand,
  }), [options, runCommand]);

  return { commandContext, runCommand };
}
