// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import type { PointerEvent as ReactPointerEvent } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Pane, Session, TmuxSnapshot, Window as TmuxWindow } from "./types";
import { paneOnScreenMovedBy, usePaneMoves } from "./usePaneMoves";
import { beginPointerDrag, type DragPayload } from "../features/dragDrop/pointerDrag";
import type { TmuxActionResult } from "../features/tmux/actions";
import type { AgentListRow } from "../features/agents/agentsList";

const session = (id: string): Session => ({ id, name: id, windowCount: 0, attachedClients: 0 });
const window_ = (id: string, sessionId: string): TmuxWindow => ({ id, sessionId, index: 0, name: id, active: true, layout: "" });
const pane = (id: string, windowId: string, sessionId: string, active = false): Pane => ({
  id, windowId, sessionId, index: 0, active, width: 80, height: 24, left: 0, top: 0, currentPath: "", currentCommand: "bash",
});

/** `$1` shows `@1` (`%1` on screen, `%2` beside it) and has `@2` (`%3`); `$2` has `@3` (`%4`). */
const snapshot: TmuxSnapshot = {
  sessions: [session("$1"), session("$2")],
  windows: [window_("@1", "$1"), window_("@2", "$1"), window_("@3", "$2")],
  panes: [pane("%1", "@1", "$1", true), pane("%2", "@1", "$1"), pane("%3", "@2", "$1"), pane("%4", "@3", "$2")],
};
const onScreen = snapshot.panes[0];

function pointer(type: string, pointerId = 1): PointerEvent {
  const event = new MouseEvent(type, { clientX: 50, clientY: 50, button: 0, buttons: type === "pointermove" ? 1 : 0 }) as unknown as PointerEvent;
  Object.defineProperty(event, "pointerId", { value: pointerId });
  return event;
}

/** Drags `payload` onto workspace `$2` and releases there. */
async function dropOnWorkspace(payload: DragPayload) {
  const row = document.createElement("div");
  row.dataset.dropWorkspace = "$2";
  row.dataset.dropHost = "local";
  document.body.append(row);
  document.elementsFromPoint = vi.fn(() => [row]);
  const press = { button: 0, pointerId: 1, clientX: 0, clientY: 0, currentTarget: document.createElement("div") };
  beginPointerDrag(press as unknown as ReactPointerEvent<HTMLElement>, payload);
  window.dispatchEvent(pointer("pointermove"));
  await act(async () => { window.dispatchEvent(pointer("pointerup")); });
}

function harness(result: TmuxActionResult, terminalOnScreen = true) {
  const performAction = vi.fn(async () => result);
  const selectPane = vi.fn();
  const focusTerminal = vi.fn();
  let moves!: ReturnType<typeof usePaneMoves>;
  function Harness() {
    moves = usePaneMoves({
      hostProfileId: "local", canMutate: true, snapshot, activeSession: snapshot.sessions[0], activePane: onScreen,
      terminalOnScreen, appTabs: [], performAction, selectPane, focusTerminal, setAppState: vi.fn(), setStatus: vi.fn(),
    });
    return null;
  }
  act(() => { create(<Harness />); });
  return { performAction, selectPane, focusTerminal, moves: () => moves };
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { callback(0); return 1; });
});
afterEach(() => {
  window.dispatchEvent(pointer("pointercancel"));
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe("paneOnScreenMovedBy", () => {
  it("names the pane on screen only when the move carries it away", () => {
    expect(paneOnScreenMovedBy({ kind: "moveWindow", sessionId: "$1", windowId: "@1", targetSessionId: "$2" }, onScreen)).toBe(onScreen);
    expect(paneOnScreenMovedBy({ kind: "breakPane", paneId: "%1", targetSessionId: "$1" }, onScreen)).toBe(onScreen);
    expect(paneOnScreenMovedBy({ kind: "moveWindow", sessionId: "$1", windowId: "@2", targetSessionId: "$2" }, onScreen)).toBeUndefined();
    expect(paneOnScreenMovedBy({ kind: "breakPane", paneId: "%2", targetSessionId: "$2" }, onScreen)).toBeUndefined();
    // A join keeps focus on the host's side, whichever pane it moves.
    expect(paneOnScreenMovedBy({ kind: "joinPane", paneId: "%1", targetPaneId: "%2", paneEdge: "left" }, onScreen)).toBeUndefined();
    expect(paneOnScreenMovedBy({ kind: "moveWindow", sessionId: "$1", windowId: "@1", targetSessionId: "$2" }, undefined)).toBeUndefined();
  });
});

describe("usePaneMoves", () => {
  it("follows the pane on screen to the workspace it was moved to", async () => {
    const { performAction, selectPane, moves } = harness({ sessionId: "$2", windowId: "@9", paneId: "%1", topologyGeneration: 2 });
    await dropOnWorkspace(moves().paneDragPayload(onScreen)!);
    expect(performAction).toHaveBeenCalledWith({ kind: "breakPane", paneId: "%1", targetSessionId: "$2" });
    expect(selectPane).toHaveBeenCalledWith({ ...onScreen, sessionId: "$2", windowId: "@9" });
  });

  it("stays where it is when something else moves", async () => {
    const { performAction, selectPane, moves } = harness({ sessionId: "$2", windowId: "@9", paneId: "%2", topologyGeneration: 2 });
    await dropOnWorkspace(moves().paneDragPayload(snapshot.panes[1])!);
    expect(performAction).toHaveBeenCalledWith({ kind: "breakPane", paneId: "%2", targetSessionId: "$2" });
    expect(selectPane).not.toHaveBeenCalled();
  });

  it("stays on a document covering the terminal when the pane beneath it moves", async () => {
    const { performAction, selectPane, moves } = harness({ sessionId: "$2", windowId: "@9", paneId: "%1", topologyGeneration: 2 }, false);
    await dropOnWorkspace(moves().paneDragPayload(onScreen)!);
    expect(performAction).toHaveBeenCalled();
    expect(selectPane).not.toHaveBeenCalled();
  });

  it("brings an agent here only from another tab", () => {
    const { moves } = harness({ topologyGeneration: 1 });
    const row = (paneId: string) => {
      const at = snapshot.panes.find((item) => item.id === paneId)!;
      return {
        routable: true,
        agent: { hostProfileId: "local", sessionId: at.sessionId, windowId: at.windowId, paneId, displayName: paneId },
      } as unknown as AgentListRow;
    };
    expect(moves().agentMoves(row("%3"))?.bringHere).toBeTypeOf("function");
    expect(moves().agentMoves(row("%2"))?.bringHere).toBeUndefined();
  });

  it("puts the keyboard back in the pane on screen after a move from the agent menu", async () => {
    const { performAction, focusTerminal, moves } = harness({ sessionId: "$1", windowId: "@1", paneId: "%3", topologyGeneration: 2 });
    const row = { routable: true, agent: { hostProfileId: "local", sessionId: "$1", windowId: "@2", paneId: "%3", displayName: "c" } };
    await act(async () => moves().agentMoves(row as unknown as AgentListRow)?.bringHere?.());
    expect(performAction).toHaveBeenCalledWith({ kind: "joinPane", paneId: "%3", targetPaneId: "%1", paneEdge: "right" });
    expect(focusTerminal).toHaveBeenCalledWith("%1");
  });

  it("carries a grabbed pane under its tab's name", () => {
    const { moves } = harness({ topologyGeneration: 1 });
    expect(moves().paneDragPayload(onScreen)).toEqual({
      kind: "pane", hostProfileId: "local", sessionId: "$1", windowId: "@1", paneId: "%1", label: "@1",
    });
  });
});
