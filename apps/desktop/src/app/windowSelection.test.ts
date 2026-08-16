import { describe, expect, it, vi } from "vitest";
import type { Window as TmuxWindow } from "./types";
import { relativeWindowReorderAction, RemoteNavigationCoordinator, requestActiveWindow, resolveActiveWindowId } from "./windowSelection";

const windows = (activeId: string): TmuxWindow[] => [
  { id: "@1", sessionId: "$1", index: 0, name: "one", active: activeId === "@1", layout: "" },
  { id: "@2", sessionId: "$1", index: 1, name: "two", active: activeId === "@2", layout: "" },
];

describe("authoritative window selection", () => {
  it("follows an external tmux active-window change even when the old window still exists", () => {
    expect(resolveActiveWindowId(windows("@2"), "@1")).toBe("@2");
  });

  it("renders a GUI-selected tab after tmux accepts selectWindow", async () => {
    const performAction = vi.fn(async () => ({ windowId: "@2", topologyGeneration: 2 }));
    const setActiveWindowId = vi.fn();
    await expect(requestActiveWindow(windows("@1"), "@1", "@2", performAction, setActiveWindowId)).resolves.toBe(true);
    expect(performAction).toHaveBeenCalledWith({ kind: "selectWindow", sessionId: "$1", windowId: "@2" });
    expect(setActiveWindowId).toHaveBeenCalledWith("@2");
  });

  it("does not change the rendered tab when disconnected/read-only gating rejects the mutation", async () => {
    const setActiveWindowId = vi.fn();
    await expect(requestActiveWindow(windows("@1"), "@1", "@2", async () => undefined, setActiveWindowId)).resolves.toBe(false);
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

  it("admits only the latest rapid remote navigation completion", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    const resolve: Array<(result: { topologyGeneration: number }) => void> = [];
    const commit = [vi.fn(), vi.fn(), vi.fn()];
    const requests = ["A", "B", "C"].map((key, index) => coordinator.navigate(
      key,
      () => new Promise((done) => resolve.push(done)),
      commit[index],
    ));
    resolve[1]({ topologyGeneration: 2 });
    resolve[0]({ topologyGeneration: 1 });
    resolve[2]({ topologyGeneration: 3 });
    await expect(Promise.all(requests)).resolves.toEqual([false, false, true]);
    expect(commit.map((callback) => callback.mock.calls.length)).toEqual([0, 0, 1]);
  });

  it("coalesces only consecutive requests for the exact same destination", async () => {
    const coordinator = new RemoteNavigationCoordinator();
    let complete!: (result: { topologyGeneration: number }) => void;
    const request = vi.fn(() => new Promise<{ topologyGeneration: number }>((done) => { complete = done; }));
    const first = coordinator.navigate("session:$1", request, vi.fn());
    const latestCommit = vi.fn();
    const second = coordinator.navigate("session:$1", request, latestCommit);
    expect(request).toHaveBeenCalledOnce();
    complete({ topologyGeneration: 2 });
    await expect(Promise.all([first, second])).resolves.toEqual([false, true]);
    expect(latestCommit).toHaveBeenCalledOnce();
  });
});
