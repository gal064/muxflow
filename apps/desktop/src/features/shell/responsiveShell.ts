import type { ShellState } from "./types";

export type SidebarCommand = "view.toggleSidebar" | "view.togglePanel" | "view.showFiles" | "view.showGit";

export interface EffectiveRails {
  sidebarOpen: boolean;
  panelOpen: boolean;
}

/**
 * Which rails are actually showing, given what the user prefers and how wide
 * the window happens to be right now.
 *
 * Below 880px the rails overlay the terminal rather than taking width from it,
 * so two open at once leave nothing of the product visible — only one may show.
 * The panel wins when both are wanted, because the panel is closed by default
 * and having it open means the user asked for it.
 *
 * This is derived, never stored, and that is the whole point. The previous
 * shape wrote the narrow-window collapse *into* the saved preferences and had
 * no inverse, so one moment at a narrow width permanently overwrote the user's
 * sidebar and panel choice — on that launch and every launch after it. A
 * viewport is not a preference.
 */
export function effectiveRails(shell: ShellState, compactViewport: boolean): EffectiveRails {
  const sidebarOpen = !shell.sidebarCollapsed;
  if (!compactViewport) return { sidebarOpen, panelOpen: shell.panelOpen };
  if (shell.panelOpen) return { sidebarOpen: false, panelOpen: true };
  return { sidebarOpen, panelOpen: false };
}

/**
 * What a rail command does to the *preference*. Width does not appear here:
 * mutual exclusion at narrow widths is `effectiveRails`'s job, so running a
 * command in a narrow window and then widening it gives back exactly the
 * arrangement the commands asked for.
 */
export function shellAfterSidebarCommand(shell: ShellState, command: SidebarCommand): ShellState {
  switch (command) {
    case "view.toggleSidebar":
      return { ...shell, sidebarCollapsed: !shell.sidebarCollapsed };
    case "view.togglePanel":
      return { ...shell, panelOpen: !shell.panelOpen };
    case "view.showFiles":
    case "view.showGit":
      // Asking for a surface opens the panel on it; it is not a toggle, so
      // running the command twice does not close what you just asked for.
      return {
        ...shell,
        panelOpen: true,
        panelSurface: command === "view.showFiles" ? "files" : "git",
      };
  }
}
