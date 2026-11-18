import { describe, expect, it, vi } from "vitest";
import { executeTool, resolvePane, summarizeCall, type ToolContext } from "./tools";
import type { PanelSessionContext } from "../shell/panelExtensions";

const session = {
  activeWindowId: "@1",
  snapshot: {
    sessions: [],
    windows: [{ id: "@1", sessionId: "$1", index: 0, name: "editor", active: true, layout: "" }],
    panes: [
      { id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true, width: 80, height: 24, left: 0, top: 0, currentPath: "/w", currentCommand: "node" },
      { id: "%2", sessionId: "$1", windowId: "@1", index: 1, active: false, width: 80, height: 24, left: 0, top: 0, currentPath: "/w", currentCommand: "vim" },
    ],
  },
} as unknown as PanelSessionContext;

function context(overrides: Partial<ToolContext> = {}): ToolContext {
  return {
    session: () => session,
    clientId: "client-1",
    profileId: "devbox",
    openPane: vi.fn(),
    sendInput: vi.fn(async () => undefined),
    readScreen: vi.fn(() => undefined),
    runAutomation: vi.fn(async () => "ran"),
    ...overrides,
  };
}

const call = (name: string, args: Record<string, unknown> = {}) =>
  ({ id: "c1", name, arguments: JSON.stringify(args) });

describe("the assistant's tools", () => {
  it("resolves the active pane by default and an index when given one", () => {
    expect(resolvePane(session, undefined)?.id).toBe("%1");
    expect(resolvePane(session, 1)?.id).toBe("%2");
    expect(resolvePane(session, 9)).toBeUndefined();
  });

  it("reads a screen without approval, asking for the default budget", async () => {
    const readScreen = vi.fn(() => "hello from the screen");
    const result = await executeTool(call("read_pane"), context({ readScreen }));
    expect(readScreen).toHaveBeenCalledWith("%1", 200);
    expect(result).toContain("Pane #0:");
    expect(result).toContain("hello from the screen");
  });

  it("clamps a requested line count and targets a pane by index", async () => {
    const readScreen = vi.fn(() => "text");
    await executeTool(call("read_pane", { pane: 1, lines: 50_000 }), context({ readScreen }));
    expect(readScreen).toHaveBeenCalledWith("%2", 2_000);
    await executeTool(call("read_pane", { lines: -4 }), context({ readScreen }));
    expect(readScreen).toHaveBeenLastCalledWith("%1", 1);
  });

  it("keeps the newest tail of a screen past the character bound", async () => {
    const screen = `${"old ".repeat(6_000)}NEWEST LINE`;
    const result = await executeTool(call("read_pane"), context({ readScreen: () => screen }));
    expect(result).toContain("older text dropped");
    expect(result.endsWith("NEWEST LINE")).toBe(true);
    expect(result.length).toBeLessThan(17_000);
  });

  it("tells the model when the pane holds no readable screen or an empty one", async () => {
    expect(await executeTool(call("read_pane"), context())).toContain("not on screen");
    const blank = context({ readScreen: () => "  \n \n" });
    expect(await executeTool(call("read_pane"), blank)).toContain("empty");
  });

  it("summarizes a read for the transcript chip", () => {
    expect(summarizeCall(call("read_pane"))).toBe("read the active pane's screen");
    expect(summarizeCall(call("read_pane", { pane: 2 }))).toBe("read pane 2");
  });
});
