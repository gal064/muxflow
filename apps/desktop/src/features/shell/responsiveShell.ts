import { TERMINAL_MIN_WIDTH, type ShellState } from "./types";

export type SidebarCommand = "view.toggleSidebar" | "view.togglePanel" | "view.showFiles" | "view.showGit";

export interface RailLayout {
  /** Whether each rail is drawn right now. */
  sidebarOpen: boolean;
  panelOpen: boolean;
  /** Whether each rail would have room if it were open; false greys out its toggle. */
  sidebarFits: boolean;
  panelFits: boolean;
}

/**
 * Which rails are actually showing, given what the user prefers and how wide
 * the window happens to be right now.
 *
 * A rail is drawn only while the terminal keeps `TERMINAL_MIN_WIDTH` beside
 * it. As the window narrows the sidebar goes first, then the panel, then it is
 * just the terminal. The panel outlasts the sidebar because it is closed by
 * default, so having it open means the user asked for it.
 *
 * This is derived, never stored, and that is the whole point. An earlier shape
 * wrote the narrow-window collapse *into* the saved preferences and had no
 * inverse, so one moment at a narrow width permanently overwrote the user's
 * sidebar and panel choice — on that launch and every launch after it. A
 * viewport is not a preference: widening gives back exactly what was open.
 */
export function railLayout(
  shell: ShellState,
  sidebarWidth: number,
  panelWidth: number,
  windowWidth: number,
): RailLayout {
  const panelFits = windowWidth - panelWidth >= TERMINAL_MIN_WIDTH;
  const panelOpen = shell.panelOpen && panelFits;
  // Measured against the *wanted* panel, not the drawn one: once the panel
  // gives way, the sidebar that gave way before it must not reappear in the
  // room the panel just left.
  const sidebarFits = windowWidth - sidebarWidth - (shell.panelOpen ? panelWidth : 0) >= TERMINAL_MIN_WIDTH;
  return { sidebarOpen: !shell.sidebarCollapsed && sidebarFits, panelOpen, sidebarFits, panelFits };
}

/**
 * What a rail command does to the *preference*. Width does not appear here:
 * what fits at the current width is `railLayout`'s job, so running a
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
