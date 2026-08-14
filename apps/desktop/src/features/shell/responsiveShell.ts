import type { ShellState } from "./types";

export type SidebarCommand = "view.toggleSidebar" | "view.togglePanel" | "view.showFiles" | "view.showGit";

/**
 * Below 880px the sidebar overlays the terminal instead of taking space from
 * it, so both rails open at once would leave nothing of the product visible.
 * Collapsing on entry, and closing the other rail whenever one opens, keeps a
 * narrow window showing a terminal.
 */
export function collapseSidebarsForCompactViewport(shell: ShellState): ShellState {
  if (shell.sidebarCollapsed && !shell.panelOpen) return shell;
  return { ...shell, sidebarCollapsed: true, panelOpen: false };
}

export function shellAfterSidebarCommand(shell: ShellState, command: SidebarCommand, compactViewport: boolean): ShellState {
  switch (command) {
    case "view.toggleSidebar": {
      const opening = shell.sidebarCollapsed;
      return {
        ...shell,
        sidebarCollapsed: !shell.sidebarCollapsed,
        panelOpen: compactViewport && opening ? false : shell.panelOpen,
      };
    }
    case "view.togglePanel": {
      const opening = !shell.panelOpen;
      return {
        ...shell,
        sidebarCollapsed: compactViewport && opening ? true : shell.sidebarCollapsed,
        panelOpen: opening,
      };
    }
    case "view.showFiles":
    case "view.showGit":
      // Asking for a surface opens the panel on it; it is not a toggle, so
      // running the command twice does not close what you just asked for.
      return {
        ...shell,
        panelOpen: true,
        panelSurface: command === "view.showFiles" ? "files" : "git",
        sidebarCollapsed: compactViewport ? true : shell.sidebarCollapsed,
      };
  }
}
