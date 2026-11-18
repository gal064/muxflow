import { describe, expect, it } from "vitest";
import type { Pane, Session, TmuxSnapshot, Window } from "../../app/types";
import { captureWorkspaceRestore, planWorkspaceRestore, sameWorkspaceRestore } from "./model";

const session = (id: string, name: string, order?: number): Session => ({
  id, name, windowCount: 0, attachedClients: 0, order,
});
const tab = (id: string, sessionId: string, index: number): Window => ({
  id, sessionId, index, name: `w${index}`, active: false, layout: "",
});
const pane = (
  id: string, sessionId: string, windowId: string, currentPath: string, active = true,
): Pane => ({
  id, sessionId, windowId, index: 0, active, width: 80, height: 24, left: 0, top: 0,
  currentPath, currentCommand: "bash",
});
const snapshot = (sessions: Session[], windows: Window[] = [], panes: Pane[] = []): TmuxSnapshot => ({
  sessions, windows, panes,
});

describe("capturing the workspace restore record", () => {
  it("records workspaces in list order with the active pane directory of every tab", () => {
    const captured = captureWorkspaceRestore(snapshot(
      [session("$2", "beta", 1), session("$1", "alpha", 0)],
      [tab("@2", "$1", 1), tab("@1", "$1", 0), tab("@3", "$2", 0)],
      [
        // Tab order follows the window index, not the order rows arrived in.
        pane("%2", "$1", "@2", "/work/api"),
        pane("%1", "$1", "@1", "/work", true),
        pane("%1b", "$1", "@1", "/work/split", false),
        pane("%3", "$2", "@3", "/srv"),
      ],
    ), "beta");
    expect(captured).toEqual({
      workspaces: [
        { name: "alpha", tabs: [{ directory: "/work" }, { directory: "/work/api" }] },
        { name: "beta", tabs: [{ directory: "/srv" }] },
      ],
      selectedSessionName: "beta",
    });
  });

  it("falls back to any pane of the tab when none reports itself active", () => {
    const captured = captureWorkspaceRestore(snapshot(
      [session("$1", "only")],
      [tab("@1", "$1", 0)],
      [pane("%1", "$1", "@1", "/fallback", false)],
    ), undefined);
    expect(captured.workspaces[0].tabs).toEqual([{ directory: "/fallback" }]);
    // No remembered selection is an absent field, not an empty one.
    expect(Object.hasOwn(captured, "selectedSessionName")).toBe(false);
  });

  it("leaves a tab tmux cannot locate at tmux's own choice rather than guessing", () => {
    const captured = captureWorkspaceRestore(snapshot(
      [session("$1", "only")],
      [tab("@1", "$1", 0), tab("@2", "$1", 1)],
      [
        // An empty path is tmux saying it could not read one; a relative one
        // is not a path the host's create would accept.
        pane("%1", "$1", "@1", ""),
        pane("%2", "$1", "@2", "relative/guess"),
      ],
    ), undefined);
    expect(captured.workspaces[0].tabs).toEqual([{}, {}]);
  });

  it("drops a workspace whose name could never be created instead of dooming the restore", () => {
    const captured = captureWorkspaceRestore(snapshot([
      session("$1", "evil\nkill-server"),
      session("$2", "keep"),
    ]), undefined);
    expect(captured.workspaces).toEqual([{ name: "keep", tabs: [] }]);
  });
});

describe("planning a host's restore", () => {
  const remembered = {
    workspaces: [
      { name: "alpha", tabs: [{ directory: "/work" }, {}, { directory: "/work/api" }] },
      { name: "beta", tabs: [] },
    ],
    selectedSessionName: "alpha",
  };

  it("plans every remembered workspace the snapshot is missing, in remembered order", () => {
    expect(planWorkspaceRestore(remembered, snapshot([]))).toEqual([
      { name: "alpha", directories: ["/work", undefined, "/work/api"] },
      { name: "beta", directories: [] },
    ]);
  });

  it("plans nothing for a workspace that is already open, by name", () => {
    // Name, not id: the case this answers is the server whose ids died with it.
    expect(planWorkspaceRestore(remembered, snapshot([session("$9", "alpha")]))).toEqual([
      { name: "beta", directories: [] },
    ]);
    expect(planWorkspaceRestore(remembered, snapshot([session("$9", "alpha"), session("$8", "beta")]))).toEqual([]);
  });

  it("plans nothing when nothing is remembered", () => {
    expect(planWorkspaceRestore(undefined, snapshot([]))).toEqual([]);
    expect(planWorkspaceRestore({ workspaces: [] }, snapshot([]))).toEqual([]);
  });
});

describe("comparing a capture against the stored record", () => {
  it("treats an absent entry and an empty capture as the same nothing", () => {
    expect(sameWorkspaceRestore(undefined, { workspaces: [] })).toBe(true);
    expect(sameWorkspaceRestore(undefined, { workspaces: [], selectedSessionName: undefined })).toBe(true);
    expect(sameWorkspaceRestore(undefined, { workspaces: [{ name: "a", tabs: [] }] })).toBe(false);
  });

  it("compares the state, not the object", () => {
    expect(sameWorkspaceRestore(
      { workspaces: [{ name: "a", tabs: [{ directory: "/x" }] }] },
      { workspaces: [{ name: "a", tabs: [{ directory: "/x" }] }] },
    )).toBe(true);
    expect(sameWorkspaceRestore(
      { workspaces: [{ name: "a", tabs: [{ directory: "/x" }] }] },
      { workspaces: [{ name: "a", tabs: [{ directory: "/y" }] }] },
    )).toBe(false);
  });
});
