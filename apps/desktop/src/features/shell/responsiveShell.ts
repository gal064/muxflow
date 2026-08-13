import type { PersistedAppState } from "./types";

type ShellState = PersistedAppState["shell"];
export type SidebarCommand = "view.toggleExplorer" | "view.toggleAgents" | "view.showExplorer" | "view.showGit";

export function collapseSidebarsForCompactViewport(shell: ShellState): ShellState {
  if (shell.explorerCollapsed && shell.agentSidebarCollapsed) return shell;
  return { ...shell, explorerCollapsed: true, agentSidebarCollapsed: true };
}

export function shellAfterSidebarCommand(shell: ShellState, command: SidebarCommand, compactViewport: boolean): ShellState {
  switch (command) {
    case "view.toggleExplorer": {
      const opening = shell.explorerCollapsed;
      return {
        ...shell,
        explorerCollapsed: !shell.explorerCollapsed,
        agentSidebarCollapsed: compactViewport && opening ? true : shell.agentSidebarCollapsed,
      };
    }
    case "view.toggleAgents": {
      const opening = shell.agentSidebarCollapsed;
      return {
        ...shell,
        explorerCollapsed: compactViewport && opening ? true : shell.explorerCollapsed,
        agentSidebarCollapsed: !shell.agentSidebarCollapsed,
      };
    }
    case "view.showExplorer":
    case "view.showGit":
      return {
        ...shell,
        explorerCollapsed: false,
        explorerSurface: command === "view.showExplorer" ? "explorer" : "git",
        agentSidebarCollapsed: compactViewport ? true : shell.agentSidebarCollapsed,
      };
  }
}
