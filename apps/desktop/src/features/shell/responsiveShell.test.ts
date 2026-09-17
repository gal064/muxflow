import { describe, expect, it } from "vitest";
import { effectiveRails, shellAfterSidebarCommand } from "./responsiveShell";
import { defaultShellState, type ShellState } from "./types";

const shell = (overrides: Partial<ShellState> = {}): ShellState => ({ ...defaultShellState, ...overrides });

describe("responsive shell state", () => {
  it("keeps a narrow window showing a terminal by showing only one rail at a time", () => {
    expect(effectiveRails(shell({ sidebarCollapsed: false, panelOpen: true }), true))
      .toEqual({ sidebarOpen: false, panelOpen: true });
    expect(effectiveRails(shell({ sidebarCollapsed: false, panelOpen: false }), true))
      .toEqual({ sidebarOpen: true, panelOpen: false });
  });

  it("shows both rails on a wide window", () => {
    expect(effectiveRails(shell({ sidebarCollapsed: false, panelOpen: true }), false))
      .toEqual({ sidebarOpen: true, panelOpen: true });
  });

  it("never writes the window's width into the user's preferences", () => {
    // The defect this replaces: entering a narrow viewport forced
    // `sidebarCollapsed: true, panelOpen: false` into persisted state with no
    // inverse, so one narrow moment overwrote the user's arrangement for good.
    // Width is now read at render time and the preference is never touched.
    const preference = shell({ sidebarCollapsed: false, panelOpen: true });
    expect(effectiveRails(preference, true)).toEqual({ sidebarOpen: false, panelOpen: true });
    // Widening restores exactly what was preferred, because nothing was lost.
    expect(effectiveRails(preference, false)).toEqual({ sidebarOpen: true, panelOpen: true });
  });

  it("closes a rail it is asked to toggle shut", () => {
    expect(shellAfterSidebarCommand(shell(), "view.toggleSidebar").sidebarCollapsed).toBe(true);
    expect(shellAfterSidebarCommand(shell({ panelOpen: true }), "view.togglePanel").panelOpen).toBe(false);
  });

  it("treats asking for a surface as opening it, not toggling it", () => {
    const first = shellAfterSidebarCommand(shell(), "view.showGit");
    expect(first).toMatchObject({ panelOpen: true, panelSurface: "git" });
    // Running the same command again must not close what it just opened.
    const second = shellAfterSidebarCommand(first, "view.showGit");
    expect(second).toMatchObject({ panelOpen: true, panelSurface: "git" });
    expect(shellAfterSidebarCommand(second, "view.showFiles").panelSurface).toBe("files");
  });

  it("keeps a rail command meaning the same thing at any width", () => {
    // Toggling the sidebar in a narrow window used to also close the panel,
    // which the user then found closed when they widened again.
    const narrowToggle = shellAfterSidebarCommand(shell({ sidebarCollapsed: true, panelOpen: true }), "view.toggleSidebar");
    expect(narrowToggle).toMatchObject({ sidebarCollapsed: false, panelOpen: true });
    expect(effectiveRails(narrowToggle, true)).toEqual({ sidebarOpen: false, panelOpen: true });
    expect(effectiveRails(narrowToggle, false)).toEqual({ sidebarOpen: true, panelOpen: true });
  });
});
