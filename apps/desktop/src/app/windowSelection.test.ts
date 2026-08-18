import { describe, expect, it, vi } from "vitest";
import type { Window as TmuxWindow } from "./types";
import { relativeWindowReorderAction, resolveActiveWindowId } from "./windowSelection";

const windows = (activeId: string): TmuxWindow[] => [
  { id: "@1", sessionId: "$1", index: 0, name: "one", active: activeId === "@1", layout: "" },
  { id: "@2", sessionId: "$1", index: 1, name: "two", active: activeId === "@2", layout: "" },
];

describe("authoritative window selection", () => {
  it("follows an external tmux active-window change even when the old window still exists", () => {
    expect(resolveActiveWindowId(windows("@2"), "@1")).toBe("@2");
  });

  it("holds an optimistic switch against a snapshot that still names the old window", () => {
    // The snapshot says @1; the switch to @2 is committed locally and its
    // request is still in flight. Following the snapshot here is exactly the
    // snap-back the guard exists to stop.
    expect(resolveActiveWindowId(windows("@1"), "@1", "@2")).toBe("@2");
  });

  it("ignores an optimistic target that no longer exists", () => {
    // A window closed under the switch is not somewhere the shell can sit.
    expect(resolveActiveWindowId(windows("@1"), "@1", "@9")).toBe("@1");
  });

  it("follows the host again once no switch is outstanding", () => {
    expect(resolveActiveWindowId(windows("@2"), "@1", undefined)).toBe("@2");
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
