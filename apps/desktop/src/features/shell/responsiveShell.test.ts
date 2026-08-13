import { describe, expect, it } from "vitest";
import { collapseSidebarsForCompactViewport, shellAfterSidebarCommand } from "./responsiveShell";
import { defaultAppState } from "./types";

describe("responsive shell", () => {
  it("collapses both sidebars when the viewport becomes compact", () => {
    expect(collapseSidebarsForCompactViewport(defaultAppState.shell)).toMatchObject({
      explorerCollapsed: true,
      agentSidebarCollapsed: true,
    });
  });

  it("keeps no more than one overlay open in a compact viewport", () => {
    const collapsed = collapseSidebarsForCompactViewport(defaultAppState.shell);
    const explorerOpen = shellAfterSidebarCommand(collapsed, "view.showGit", true);
    expect(explorerOpen).toMatchObject({ explorerCollapsed: false, agentSidebarCollapsed: true, explorerSurface: "git" });
    expect(shellAfterSidebarCommand(explorerOpen, "view.toggleAgents", true)).toMatchObject({
      explorerCollapsed: true,
      agentSidebarCollapsed: false,
    });
  });

  it("preserves independent sidebars outside a compact viewport", () => {
    const result = shellAfterSidebarCommand(defaultAppState.shell, "view.toggleExplorer", false);
    expect(result).toMatchObject({ explorerCollapsed: true, agentSidebarCollapsed: false });
  });
});
