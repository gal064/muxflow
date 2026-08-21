import { describe, expect, it } from "vitest";
import type { Pane } from "./types";
import { currentTerminalFilePane } from "./terminalFileOpenRoute";

describe("currentTerminalFilePane", () => {
  it("returns the unchanged live pane at the host-confirmed generation", () => {
    const pane = makePane();
    expect(currentTerminalFilePane(pane, [pane], 7, "7")).toBe(pane);
  });

  it.each([
    ["generation", makePane(), 8, "7"],
    ["cwd", makePane({ currentPath: "/other" }), 7, "7"],
    ["window", makePane({ windowId: "@2" }), 7, "7"],
    ["session", makePane({ sessionId: "$2" }), 7, "7"],
  ])("rejects a stale %s route", (_reason, live, generation, resolvedGeneration) => {
    expect(currentTerminalFilePane(makePane(), [live], generation, resolvedGeneration)).toBeUndefined();
  });

  it("rejects a pane removed while resolution was in flight", () => {
    expect(currentTerminalFilePane(makePane(), [], 7, "7")).toBeUndefined();
  });
});

function makePane(overrides: Partial<Pane> = {}): Pane {
  return {
    id: "%1", sessionId: "$1", windowId: "@1", index: 0, active: true,
    width: 80, height: 24, left: 0, top: 0, currentPath: "/repo", currentCommand: "zsh",
    ...overrides,
  };
}
