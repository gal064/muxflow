import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { TitleBar } from "./TitleBar";

const noop = () => {};
const props = {
  platform: "linux" as const,
  workspaceName: "muxflow",
  sidebarOpen: true,
  panelOpen: false,
  sidebarFits: true,
  panelFits: true,
  unread: 0,
  canJump: false,
  canCreateWorkspace: true,
  canGoBack: false,
  canGoForward: false,
  windowControls: { maximized: false, onMinimize: noop, onToggleMaximize: noop, onClose: noop },
  onBack: noop,
  onForward: noop,
  onToggleSidebar: noop,
  onTogglePanel: noop,
  onNewWorkspace: noop,
  onBell: noop,
  onUpdate: noop,
  onOpenSettings: noop,
};

/** The accessible names in document order. */
function labels(markup: string): string[] {
  return [...markup.matchAll(/aria-label="([^"]+)"/g)].map((match) => match[1]);
}

describe("title bar", () => {
  it("keeps today's arrangement without tabs", () => {
    const markup = renderToStaticMarkup(<TitleBar {...props} />);
    expect(markup).toContain("muxflow");
    expect(labels(markup)).toEqual([
      "Settings", "Toggle sidebar", "Back", "Forward", "No agents waiting",
      "New workspace", "Toggle right panel", "Minimize", "Maximize", "Close window",
    ]);
  });

  it("with tabs, leads with the window buttons and the app's controls, and ends with the rail toggles", () => {
    const markup = renderToStaticMarkup(<TitleBar {...props} tabs={<div aria-label="Tabs" />} />);
    expect(markup).not.toContain("muxflow");
    expect(labels(markup)).toEqual([
      "Close window", "Minimize", "Maximize", "Settings", "No agents waiting", "Back", "Forward",
      "Tabs", "Toggle sidebar", "Toggle right panel",
    ]);
  });

  it("with tabs, keeps New workspace only while the sidebar that holds it is shut", () => {
    const shut = renderToStaticMarkup(<TitleBar {...props} sidebarOpen={false} tabs={<div />} />);
    expect(labels(shut)).toContain("New workspace");
  });
});
