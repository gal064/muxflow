import { describe, expect, it } from "vitest";
import type { Pane, Session, Window as TmuxWindow } from "../../app/types";
import { ALREADY_OPEN, CLOSES_WORKSPACE, NO_DOCUMENT_SPLITS, ONE_PANE_TAB_ONLY, SAME_HOST_ONLY, planDrop, type DropContext } from "./dropRules";
import type { DragPayload } from "./pointerDrag";

const session = (id: string, name = id): Session => ({ id, name, windowCount: 0, attachedClients: 0 });
const window = (id: string, sessionId: string, index: number): TmuxWindow => ({ id, sessionId, index, name: id, active: false, layout: "" });
const pane = (id: string, windowId: string, sessionId: string): Pane => ({
  id, windowId, sessionId, index: 0, active: false, width: 80, height: 24, left: 0, top: 0, currentPath: "", currentCommand: "bash",
});

/**
 * `$1` holds `@1` (`%1`, `%2` split) and `@2` (`%3`); `$2` holds only `@3`
 * (`%4`), so it is the workspace a move must not empty.
 */
const context: DropContext = {
  hostProfileId: "local",
  sessions: [session("$1", "one"), session("$2", "two")],
  windows: [window("@1", "$1", 0), window("@2", "$1", 1), window("@3", "$2", 0)],
  panes: [pane("%1", "@1", "$1"), pane("%2", "@1", "$1"), pane("%3", "@2", "$1"), pane("%4", "@3", "$2")],
  appTabs: [{ id: "readme", sessionId: "$1", kind: "markdown", resource: "/r/README.md" }],
};

const terminalTab = (windowId: string, sessionId = "$1"): DragPayload => ({ kind: "terminalTab", hostProfileId: "local", sessionId, windowId, label: windowId });
const agent = (paneId: string): DragPayload => ({ kind: "agent", hostProfileId: "local", sessionId: "", windowId: "", paneId, label: paneId });
const appTab: DragPayload = { kind: "appTab", hostProfileId: "local", sessionId: "$1", tabId: "readme", label: "README.md" };

describe("planDrop", () => {
  it("reorders a terminal tab against another terminal tab in its workspace", () => {
    expect(planDrop(terminalTab("@2"), { kind: "tab", tabKey: "terminal:@1", side: "before" }, context)).toEqual({
      kind: "action",
      action: { kind: "reorderWindow", sessionId: "$1", windowId: "@2", targetWindowId: "@1", relativePosition: "before" },
    });
    expect(planDrop(terminalTab("@2"), { kind: "tab", tabKey: "terminal:@2", side: "after" }, context)).toBeUndefined();
    expect(planDrop(terminalTab("@2"), { kind: "tab", tabKey: "app:readme", side: "after" }, context)).toBeUndefined();
  });

  it("keeps a reorder inside the pinned group or outside it, where its marker can be honoured", () => {
    const pinned = { ...context, windows: context.windows.map((item) => item.id === "@1" ? { ...item, pinned: true } : item) };
    expect(planDrop(terminalTab("@2"), { kind: "tab", tabKey: "terminal:@1", side: "before" }, pinned)).toBeUndefined();
    expect(planDrop(terminalTab("@1"), { kind: "tab", tabKey: "terminal:@2", side: "after" }, pinned)).toBeUndefined();
  });

  it("moves a terminal tab to another workspace unless it is its workspace's only tab", () => {
    expect(planDrop(terminalTab("@2"), { kind: "workspace", hostProfileId: "local", sessionId: "$2" }, context)).toEqual({
      kind: "action",
      action: { kind: "moveWindow", sessionId: "$1", windowId: "@2", targetSessionId: "$2" },
    });
    expect(planDrop(terminalTab("@2"), { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context)).toBeUndefined();
    expect(planDrop(terminalTab("@3", "$2"), { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context))
      .toEqual({ kind: "refuse", reason: CLOSES_WORKSPACE });
  });

  it("turns only a single-pane terminal tab into a split", () => {
    expect(planDrop(terminalTab("@2"), { kind: "pane", paneId: "%4", edge: "left" }, context)).toEqual({
      kind: "action",
      action: { kind: "joinPane", paneId: "%3", targetPaneId: "%4", paneEdge: "left" },
    });
    expect(planDrop(terminalTab("@1"), { kind: "pane", paneId: "%4", edge: "left" }, context))
      .toEqual({ kind: "refuse", reason: ONE_PANE_TAB_ONLY });
    expect(planDrop(terminalTab("@3", "$2"), { kind: "pane", paneId: "%1", edge: "top" }, context))
      .toEqual({ kind: "refuse", reason: CLOSES_WORKSPACE });
    expect(planDrop(terminalTab("@1"), { kind: "pane", paneId: "%2", edge: "top" }, context)).toBeUndefined();
  });

  it("keeps document tabs local and out of splits", () => {
    expect(planDrop(appTab, { kind: "tab", tabKey: "app:notes", side: "after" }, context))
      .toEqual({ kind: "moveAppTab", tabId: "readme", targetTabId: "notes", side: "after" });
    expect(planDrop(appTab, { kind: "tab", tabKey: "terminal:@1", side: "after" }, context)).toBeUndefined();
    expect(planDrop(appTab, { kind: "workspace", hostProfileId: "local", sessionId: "$2" }, context))
      .toEqual({ kind: "moveAppTabToWorkspace", tabId: "readme", session: context.sessions[1] });
    expect(planDrop(appTab, { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context)).toBeUndefined();
    const openThere = { ...context, appTabs: [...context.appTabs, { id: "readme-2", sessionId: "$2", kind: "markdown" as const, resource: "/r/README.md" }] };
    expect(planDrop(appTab, { kind: "workspace", hostProfileId: "local", sessionId: "$2" }, openThere)).toEqual({ kind: "refuse", reason: ALREADY_OPEN });
    expect(planDrop(appTab, { kind: "pane", paneId: "%1", edge: "left" }, context)).toEqual({ kind: "refuse", reason: NO_DOCUMENT_SPLITS });
  });

  it("joins an agent's pane beside another window's pane, judged by where the pane is now", () => {
    expect(planDrop(agent("%2"), { kind: "pane", paneId: "%3", edge: "bottom" }, context)).toEqual({
      kind: "action",
      action: { kind: "joinPane", paneId: "%2", targetPaneId: "%3", paneEdge: "bottom" },
    });
    expect(planDrop(agent("%2"), { kind: "pane", paneId: "%1", edge: "left" }, context)).toBeUndefined();
    expect(planDrop(agent("%4"), { kind: "pane", paneId: "%1", edge: "left" }, context)).toEqual({ kind: "refuse", reason: CLOSES_WORKSPACE });
    expect(planDrop(agent("%9"), { kind: "pane", paneId: "%1", edge: "left" }, context)).toBeUndefined();
  });

  it("moves an agent alone in its window as that window, and breaks out one that shares", () => {
    expect(planDrop(agent("%3"), { kind: "workspace", hostProfileId: "local", sessionId: "$2" }, context)).toEqual({
      kind: "action",
      action: { kind: "moveWindow", sessionId: "$1", windowId: "@2", targetSessionId: "$2" },
    });
    expect(planDrop(agent("%3"), { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context)).toBeUndefined();
    expect(planDrop(agent("%2"), { kind: "workspace", hostProfileId: "local", sessionId: "$2" }, context)).toEqual({
      kind: "action",
      action: { kind: "breakPane", paneId: "%2", targetSessionId: "$2" },
    });
    // Its own workspace: the split becomes a tab there.
    expect(planDrop(agent("%2"), { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context)).toEqual({
      kind: "action",
      action: { kind: "breakPane", paneId: "%2", targetSessionId: "$1" },
    });
    expect(planDrop(agent("%4"), { kind: "workspace", hostProfileId: "local", sessionId: "$1" }, context))
      .toEqual({ kind: "refuse", reason: CLOSES_WORKSPACE });
  });

  it("never moves anything between hosts", () => {
    expect(planDrop(terminalTab("@2"), { kind: "workspace", hostProfileId: "remote", sessionId: "$2" }, context))
      .toEqual({ kind: "refuse", reason: SAME_HOST_ONLY });
    expect(planDrop({ ...agent("%2"), hostProfileId: "remote" }, { kind: "pane", paneId: "%3", edge: "left" }, context))
      .toEqual({ kind: "refuse", reason: SAME_HOST_ONLY });
  });
});
