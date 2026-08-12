import { describe, expect, it } from "vitest";
import { isDestructiveTmuxAction, isStaleTmuxTopologyError, toWireTmuxAction } from "./actions";

describe("tmux action boundary", () => {
  it("carries authoritative identity/generation and defaults confirmation off", () => {
    expect(toWireTmuxAction({ kind: "closePane", paneId: "%7" }, {
      serverIdentity: "tmux:one", generation: 42,
    })).toMatchObject({
      kind: "closePane", pane_id: "%7", expected_server_identity: "tmux:one",
      expected_generation: 42, confirmed: false,
    });
  });

  it("classifies every topology kill as destructive", () => {
    expect(["closeSession", "closeWindow", "closePane"].every((kind) =>
      isDestructiveTmuxAction({ kind: kind as "closePane" }),
    )).toBe(true);
    expect(isDestructiveTmuxAction({ kind: "splitPaneRight" })).toBe(false);
  });

  it("recognizes only authoritative stale-topology failures as retryable", () => {
    expect(isStaleTmuxTopologyError("stale topology: generation changed")).toBe(true);
    expect(isStaleTmuxTopologyError(new Error("generation changed"))).toBe(true);
    expect(isStaleTmuxTopologyError("permission denied")).toBe(false);
  });

  it("addresses window reorder relative to a distinct stable window ID", () => {
    expect(toWireTmuxAction({
      kind: "reorderWindow",
      sessionId: "$1",
      windowId: "@5",
      targetWindowId: "@3",
      relativePosition: "before",
    }, { serverIdentity: "tmux:one", generation: 42 })).toMatchObject({
      kind: "reorderWindow",
      session_id: "$1",
      window_id: "@5",
      target_window_id: "@3",
      relative_position: "before",
      index: 0,
    });
  });

  it("rejects missing, identical, or unspecified relative window targets at the frontend boundary", () => {
    const precondition = { serverIdentity: "tmux:one", generation: 42 };
    expect(() => toWireTmuxAction({ kind: "reorderWindow", sessionId: "$1", windowId: "@5" }, precondition))
      .toThrow("distinct target");
    expect(() => toWireTmuxAction({
      kind: "reorderWindow", sessionId: "$1", windowId: "@5", targetWindowId: "@5", relativePosition: "after",
    }, precondition)).toThrow("distinct target");
  });
});
