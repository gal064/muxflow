import { describe, expect, it } from "vitest";
import { collapseSidebarsForCompactViewport, shellAfterSidebarCommand } from "./responsiveShell";
import { defaultShellState, type ShellState } from "./types";

const shell = (overrides: Partial<ShellState> = {}): ShellState => ({ ...defaultShellState, ...overrides });

describe("responsive shell state", () => {
  it("keeps a narrow window showing a terminal by opening only one rail at a time", () => {
    const withPanel = shellAfterSidebarCommand(shell({ sidebarCollapsed: true, panelOpen: true }), "view.toggleSidebar", true);
    expect(withPanel).toMatchObject({ sidebarCollapsed: false, panelOpen: false });
    const withSidebar = shellAfterSidebarCommand(shell({ sidebarCollapsed: false, panelOpen: false }), "view.togglePanel", true);
    expect(withSidebar).toMatchObject({ sidebarCollapsed: true, panelOpen: true });
  });

  it("leaves both rails alone on a wide window", () => {
    const wide = shellAfterSidebarCommand(shell({ sidebarCollapsed: true, panelOpen: true }), "view.toggleSidebar", false);
    expect(wide).toMatchObject({ sidebarCollapsed: false, panelOpen: true });
  });

  it("closes a rail it is asked to toggle shut, on any width", () => {
    expect(shellAfterSidebarCommand(shell(), "view.toggleSidebar", false).sidebarCollapsed).toBe(true);
    expect(shellAfterSidebarCommand(shell({ panelOpen: true }), "view.togglePanel", false).panelOpen).toBe(false);
  });

  it("treats asking for a surface as opening it, not toggling it", () => {
    const first = shellAfterSidebarCommand(shell(), "view.showGit", false);
    expect(first).toMatchObject({ panelOpen: true, panelSurface: "git" });
    // Running the same command again must not close what it just opened.
    const second = shellAfterSidebarCommand(first, "view.showGit", false);
    expect(second).toMatchObject({ panelOpen: true, panelSurface: "git" });
    expect(shellAfterSidebarCommand(second, "view.showFiles", false).panelSurface).toBe("files");
  });

  it("collapses both rails when the window becomes narrow, and is idempotent", () => {
    const collapsed = collapseSidebarsForCompactViewport(shell({ panelOpen: true }));
    expect(collapsed).toMatchObject({ sidebarCollapsed: true, panelOpen: false });
    expect(collapseSidebarsForCompactViewport(collapsed)).toBe(collapsed);
  });
});
