import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { enablePerfProbe, perfCounterSnapshot, resetPerfProbe } from "../../perf/probe";
import { isDestructiveTmuxAction, isStaleTmuxTopologyError, requestTmuxAction, toWireTmuxAction } from "./actions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  resetPerfProbe();
  vi.mocked(invoke).mockReset();
});

describe("tmux action boundary", () => {
  it("measures the exact client-and-action boundary passed to Tauri", async () => {
    enablePerfProbe(async () => undefined);
    vi.mocked(invoke).mockResolvedValue({ topologyGeneration: 43 });

    await requestTmuxAction("client", { kind: "createWindow", sessionId: "$1", name: "work" }, {
      serverIdentity: "tmux:one", generation: 42,
    });

    const boundary = vi.mocked(invoke).mock.calls[0][1];
    const exactBoundaryBytes = new TextEncoder().encode(JSON.stringify(boundary)).byteLength;
    expect(perfCounterSnapshot()).toMatchObject({
      "desktop.hostRequestBytes": exactBoundaryBytes,
      "tmux.hostRequestBytes": exactBoundaryBytes,
      "tmux.action.requestBytes": exactBoundaryBytes,
    });
  });

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
