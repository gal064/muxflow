import { describe, expect, it } from "vitest";
import { railLayout, shellAfterSidebarCommand } from "./responsiveShell";
import { defaultShellState, TERMINAL_MIN_WIDTH, type ShellState } from "./types";

const shell = (overrides: Partial<ShellState> = {}): ShellState => ({ ...defaultShellState, ...overrides });
const both = shell({ sidebarCollapsed: false, panelOpen: true });
const shown = (preference: ShellState, windowWidth: number, sidebarWidth = 240, panelWidth = 300) => {
  const { sidebarOpen, panelOpen } = railLayout(preference, sidebarWidth, panelWidth, windowWidth);
  return { sidebarOpen, panelOpen };
};

describe("responsive shell state", () => {
  it("shows both rails while the terminal keeps its minimum beside them", () => {
    expect(shown(both, 240 + 300 + TERMINAL_MIN_WIDTH)).toEqual({ sidebarOpen: true, panelOpen: true });
  });

  it("hides the sidebar first, then the panel, then leaves just the terminal", () => {
    expect(shown(both, 240 + 300 + TERMINAL_MIN_WIDTH - 1)).toEqual({ sidebarOpen: false, panelOpen: true });
    expect(shown(both, 300 + TERMINAL_MIN_WIDTH)).toEqual({ sidebarOpen: false, panelOpen: true });
    expect(shown(both, 300 + TERMINAL_MIN_WIDTH - 1)).toEqual({ sidebarOpen: false, panelOpen: false });
    expect(shown(both, 320)).toEqual({ sidebarOpen: false, panelOpen: false });
    // The room the panel left is not handed back to the sidebar that went first:
    // at 750 the sidebar alone would fit, but it stays hidden behind the panel.
    expect(shown(both, 750)).toEqual({ sidebarOpen: false, panelOpen: false });
  });

  it("gives the sidebar the panel's room when the panel is closed", () => {
    const sidebarOnly = shell({ sidebarCollapsed: false, panelOpen: false });
    expect(shown(sidebarOnly, 240 + TERMINAL_MIN_WIDTH)).toEqual({ sidebarOpen: true, panelOpen: false });
    expect(shown(sidebarOnly, 240 + TERMINAL_MIN_WIDTH - 1)).toEqual({ sidebarOpen: false, panelOpen: false });
  });

  it("measures against the rails' own widths, not a fixed breakpoint", () => {
    // A sidebar dragged wide gives way sooner than a default one.
    expect(shown(both, 1_200, 240, 300)).toEqual({ sidebarOpen: true, panelOpen: true });
    expect(shown(both, 1_200, 400, 400)).toEqual({ sidebarOpen: false, panelOpen: true });
  });

  it("never opens a rail the user closed, however wide the window", () => {
    expect(shown(shell({ sidebarCollapsed: true, panelOpen: false }), 4_000)).toEqual({ sidebarOpen: false, panelOpen: false });
  });

  it("reports which rails would fit, so their toggles can say why they are off", () => {
    expect(railLayout(both, 240, 300, 900)).toMatchObject({ sidebarFits: false, panelFits: true });
    expect(railLayout(both, 240, 300, 700)).toMatchObject({ sidebarFits: false, panelFits: false });
    // Without the panel beside it the sidebar has room at 900.
    expect(railLayout(shell({ panelOpen: false }), 240, 300, 900)).toMatchObject({ sidebarFits: true, panelFits: true });
  });

  it("never writes the window's width into the user's preferences", () => {
    // The defect this guards: entering a narrow viewport once forced
    // `sidebarCollapsed: true, panelOpen: false` into persisted state with no
    // inverse, so one narrow moment overwrote the user's arrangement for good.
    // Width is read at render time and the preference is never touched.
    const preference = shell({ sidebarCollapsed: false, panelOpen: true });
    const before = { ...preference };
    expect(shown(preference, 400)).toEqual({ sidebarOpen: false, panelOpen: false });
    expect(preference).toEqual(before);
    // Widening restores exactly what was preferred, because nothing was lost.
    expect(shown(preference, 1_280)).toEqual({ sidebarOpen: true, panelOpen: true });
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
    expect(shown(narrowToggle, 900)).toEqual({ sidebarOpen: false, panelOpen: true });
    expect(shown(narrowToggle, 1_280)).toEqual({ sidebarOpen: true, panelOpen: true });
  });
});
