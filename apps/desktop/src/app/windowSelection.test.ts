import { describe, expect, it, vi } from "vitest";
import type { Window as TmuxWindow } from "./types";
import { relativeWindowReorderAction, requestActiveWindow, resolveActiveWindowId } from "./windowSelection";

const windows = (activeId: string): TmuxWindow[] => [
  { id: "@1", sessionId: "$1", index: 0, name: "one", active: activeId === "@1", layout: "" },
  { id: "@2", sessionId: "$1", index: 1, name: "two", active: activeId === "@2", layout: "" },
];

describe("authoritative window selection", () => {
  it("follows an external tmux active-window change even when the old window still exists", () => {
    expect(resolveActiveWindowId(windows("@2"), "@1")).toBe("@2");
  });

  it("renders a GUI-selected tab after tmux accepts selectWindow", async () => {
    const performAction = vi.fn(async () => true);
    const setActiveWindowId = vi.fn();
    await expect(requestActiveWindow(windows("@1"), "@1", "@2", performAction, setActiveWindowId)).resolves.toBe(true);
    expect(performAction).toHaveBeenCalledWith({ kind: "selectWindow", sessionId: "$1", windowId: "@2" });
    expect(setActiveWindowId).toHaveBeenCalledWith("@2");
  });

  it("does not change the rendered tab when disconnected/read-only gating rejects the mutation", async () => {
    const setActiveWindowId = vi.fn();
    await expect(requestActiveWindow(windows("@1"), "@1", "@2", async () => false, setActiveWindowId)).resolves.toBe(false);
    expect(setActiveWindowId).not.toHaveBeenCalled();
  });

  it("moves left and right relative to adjacent stable window IDs", () => {
    const unordered = [windows("@1")[1], windows("@1")[0]];
    expect(relativeWindowReorderAction(unordered, "@2", "left")).toEqual({
      kind: "reorderWindow", sessionId: "$1", windowId: "@2",
      targetWindowId: "@1", relativePosition: "before",
    });
    expect(relativeWindowReorderAction(unordered, "@1", "right")).toEqual({
      kind: "reorderWindow", sessionId: "$1", windowId: "@1",
      targetWindowId: "@2", relativePosition: "after",
    });
  });

  it("does not emit a reorder at either ordered-list boundary", () => {
    expect(relativeWindowReorderAction(windows("@1"), "@1", "left")).toBeUndefined();
    expect(relativeWindowReorderAction(windows("@1"), "@2", "right")).toBeUndefined();
  });

  it("never chooses an adjacent window from another session", () => {
    const crossSession = windows("@1");
    crossSession[1] = { ...crossSession[1], sessionId: "$2" };
    expect(relativeWindowReorderAction(crossSession, "@1", "right")).toBeUndefined();
  });
});
