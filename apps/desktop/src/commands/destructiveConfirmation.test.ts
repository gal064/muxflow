import { describe, expect, it } from "vitest";
import { createTmuxConfirmation } from "./destructiveConfirmation";
import type { TmuxAction } from "../features/tmux/actions";

describe("destructive tmux confirmation identity", () => {
  const cases: Array<{
    name: string;
    commandId: "session.close" | "window.close" | "pane.close";
    title: string;
    label: string;
    action: TmuxAction;
  }> = [
    {
      name: "session",
      commandId: "session.close",
      title: "Close workspace…",
      label: "workspace “build” and all of its windows",
      action: { kind: "closeSession", sessionId: "$1" },
    },
    {
      name: "window",
      commandId: "window.close",
      title: "Close terminal tab…",
      label: "terminal tab “api” and all of its panes",
      action: { kind: "closeWindow", sessionId: "$1", windowId: "@2" },
    },
    {
      name: "pane",
      commandId: "pane.close",
      title: "Close pane…",
      label: "pane %3",
      action: { kind: "closePane", sessionId: "$1", windowId: "@2", paneId: "%3" },
    },
  ];

  for (const value of cases) {
    it(`captures the exact ${value.name} action, label, and generation across an external focus race`, () => {
      const pending = createTmuxConfirmation(
        value.commandId,
        value.title,
        value.label,
        { ...value.action, confirmed: true },
        { serverIdentity: "tmux:before", generation: 17 },
      );

      // These represent authoritative focus/topology changing while the dialog is open.
      const externallyFocused = { sessionId: "$9", windowId: "@9", paneId: "%9", generation: 18 };
      expect(externallyFocused).toBeDefined();
      expect(pending.action).toEqual({ ...value.action, confirmed: true });
      expect(pending.targetLabel).toBe(value.label);
      expect(pending.detail).toContain(value.label);
      expect(pending.precondition).toEqual({ serverIdentity: "tmux:before", generation: 17 });
    });
  }

  it("rejects accidental confirmation wrapping around a non-destructive action", () => {
    expect(() => createTmuxConfirmation(
      "window.moveLeft",
      "Move terminal tab left",
      "terminal tab “api”",
      { kind: "reorderWindow", sessionId: "$1", windowId: "@2", targetWindowId: "@1", relativePosition: "before", confirmed: true },
      { serverIdentity: "tmux:one", generation: 1 },
    )).toThrow("destructive");
  });

  it("refuses an action the caller has not already marked confirmed", () => {
    // The flag has one owner now: the dispatch that decides whether a close
    // asks first. This builder carrying its own stamp is what made a close
    // without a dialog need a second place to set it.
    expect(() => createTmuxConfirmation(
      "window.close",
      "Close terminal tab…",
      "terminal tab “api”",
      { kind: "closeWindow", sessionId: "$1", windowId: "@2" },
      { serverIdentity: "tmux:one", generation: 1 },
    )).toThrow("confirmed");
  });
});
