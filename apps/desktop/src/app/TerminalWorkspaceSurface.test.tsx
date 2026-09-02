// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";
import stylesCss from "../styles.css?raw";
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
  cacheScope: "local",
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

  it("draws handles only on real internal split boundaries", async () => {
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<TerminalWorkspaceSurface {...surfaceProps("shell")} />); });
    expect(renderer.root.findAllByProps({ className: "divider-handle horizontal" })).toHaveLength(0);
    expect(renderer.root.findAllByProps({ className: "divider-handle vertical" })).toHaveLength(0);
    await act(async () => renderer.unmount());

    const right: Pane = { ...pane, id: "%2", index: 1, active: false, left: 40, width: 40 };
    const left: Pane = { ...pane, width: 39 };
    await act(async () => {
      renderer = create(<TerminalWorkspaceSurface {...surfaceProps("shell")}
        activePane={left} grid={{ width: 80, height: 24 }} mountedPanes={[left, right]} panes={[left, right]}
      />);
    });
    expect(renderer.root.findAllByProps({ className: "divider-handle horizontal" })).toHaveLength(1);
    expect(renderer.root.findAllByProps({ className: "divider-handle vertical" })).toHaveLength(0);
    await act(async () => renderer.unmount());

    const bottom: Pane = { ...pane, id: "%3", index: 1, active: false, top: 12, height: 12 };
    const top: Pane = { ...pane, height: 11 };
    await act(async () => {
      renderer = create(<TerminalWorkspaceSurface {...surfaceProps("shell")}
        activePane={top} grid={{ width: 80, height: 24 }} mountedPanes={[top, bottom]} panes={[top, bottom]}
      />);
    });
    expect(renderer.root.findAllByProps({ className: "divider-handle horizontal" })).toHaveLength(0);
    expect(renderer.root.findAllByProps({ className: "divider-handle vertical" })).toHaveLength(1);
    await act(async () => renderer.unmount());
  });

  it("uses the app hairline for a continuous resting divider", () => {
    expect(stylesCss).toContain('.divider-handle::after { content: ""; position: absolute; background: var(--chrome-hairline);');
    expect(stylesCss).toContain(".divider-handle.horizontal::after { top: 0; right: 0; width: 1px; height: 100%; }");
    expect(stylesCss).toContain(".divider-handle.vertical::after { bottom: 0; left: 0; width: 100%; height: 1px; }");
  });
});
