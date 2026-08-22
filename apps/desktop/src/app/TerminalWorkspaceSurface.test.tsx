// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";
import type { Pane } from "./types";

const terminal = vi.hoisted(() => ({ renders: 0 }));
vi.mock("../features/terminal/TerminalPane", () => ({
  TerminalPane: () => { terminal.renders += 1; return <div>terminal</div>; },
}));

import { TerminalWorkspaceSurface } from "./TerminalWorkspaceSurface";

const pane: Pane = {
  id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true,
  width: 80, height: 24, left: 0, top: 0, currentPath: "/tmp", currentCommand: "zsh",
};

const surfaceProps = (windowName: string) => ({
  appFocused: true,
  activePane: pane,
  activeWindow: { id: "@1", sessionId: "$1", index: 0, name: windowName, active: true, layout: "" },
  beginDividerDrag: vi.fn(),
  clientId: "client-a",
  controllers: { current: new Map() },
  focusPane: vi.fn(),
  grid: { width: 80, height: 24 },
  handleInput: vi.fn(),
  hub: {},
  mountedPanes: [pane],
  onMeasurements: vi.fn(),
  panes: [pane],
  performAction: vi.fn(async () => undefined),
  setStatus: vi.fn(),
  surfaceRef: vi.fn(),
  terminalTransferClient: {},
  terminalTransferRegistry: {},
}) as unknown as ComponentProps<typeof TerminalWorkspaceSurface>;

describe("TerminalWorkspaceSurface", () => {
  it("does not revisit live panes for an unrelated root notice render", async () => {
    terminal.renders = 0;
    const props = surfaceProps("shell");
    let notice = "first";
    const root = () => <><span>{notice}</span><TerminalWorkspaceSurface {...props} /></>;
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(root()); });
    expect(terminal.renders).toBe(1);

    notice = "second";
    await act(async () => { renderer.update(root()); });

    expect(terminal.renders).toBe(1);
    await act(async () => renderer.unmount());
  });

  it("names the region by the tab title without the agent's status ticker", async () => {
    // The raw tmux name carries the CLI's animated status glyph, so leaving it
    // in disagrees with the tab button that labels this panel and re-announces
    // the region at the CLI's redraw rate.
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<TerminalWorkspaceSurface {...surfaceProps("✳ Fix tests")} />); });
    expect(renderer.root.findByProps({ className: "terminal-window" }).props["aria-label"])
      .toBe("Terminal tab Fix tests");
    await act(async () => renderer.unmount());

    await act(async () => { renderer = create(<TerminalWorkspaceSurface {...surfaceProps("shell")} />); });
    expect(renderer.root.findByProps({ className: "terminal-window" }).props["aria-label"])
      .toBe("Terminal tab shell");
    await act(async () => renderer.unmount());
  });
});
