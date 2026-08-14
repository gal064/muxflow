import { describe, expect, it } from "vitest";
import {
  canGoBack,
  canGoForward,
  emptyFocusHistory,
  FOCUS_HISTORY_LIMIT,
  pruneFocusHistory,
  stepFocus,
  visitFocus,
} from "./focusHistory";

const at = (sessionId: string, windowId?: string) => ({ sessionId, windowId });

describe("focus history", () => {
  it("goes back and forward to exactly where it was", () => {
    let history = visitFocus(visitFocus(visitFocus(emptyFocusHistory, at("$1", "@1")), at("$1", "@2")), at("$2", "@5"));
    expect(canGoBack(history)).toBe(true);
    expect(canGoForward(history)).toBe(false);
    const back = stepFocus(history, "back");
    expect(back.point).toEqual(at("$1", "@2"));
    history = back.history;
    expect(canGoForward(history)).toBe(true);
    expect(stepFocus(history, "forward").point).toEqual(at("$2", "@5"));
  });

  it("does not record standing still", () => {
    const once = visitFocus(emptyFocusHistory, at("$1", "@1"));
    expect(visitFocus(once, at("$1", "@1"))).toBe(once);
    expect(once.entries).toHaveLength(1);
  });

  it("truncates the forward branch when you navigate after going back", () => {
    let history = visitFocus(visitFocus(visitFocus(emptyFocusHistory, at("$1")), at("$2")), at("$3"));
    history = stepFocus(history, "back").history;
    history = visitFocus(history, at("$4"));
    expect(history.entries.map((entry) => entry.sessionId)).toEqual(["$1", "$2", "$4"]);
    expect(canGoForward(history)).toBe(false);
  });

  it("stays bounded, and keeps the cursor on the newest entry when it trims", () => {
    let history = emptyFocusHistory;
    for (let index = 0; index < FOCUS_HISTORY_LIMIT + 20; index += 1) history = visitFocus(history, at(`$${index}`));
    expect(history.entries).toHaveLength(FOCUS_HISTORY_LIMIT);
    expect(history.cursor).toBe(FOCUS_HISTORY_LIMIT - 1);
    expect(history.entries.at(-1)).toEqual(at(`$${FOCUS_HISTORY_LIMIT + 19}`));
  });

  it("never offers to navigate to something another tmux client closed", () => {
    let history = visitFocus(visitFocus(visitFocus(emptyFocusHistory, at("$1")), at("$2")), at("$3"));
    history = stepFocus(history, "back").history;
    const pruned = pruneFocusHistory(history, (point) => point.sessionId !== "$1");
    expect(pruned.entries.map((entry) => entry.sessionId)).toEqual(["$2", "$3"]);
    // The cursor still points at the same place it did before the prune.
    expect(pruned.entries[pruned.cursor]).toEqual(at("$2"));
  });

  it("has nowhere to go when it is empty", () => {
    expect(canGoBack(emptyFocusHistory)).toBe(false);
    expect(canGoForward(emptyFocusHistory)).toBe(false);
    expect(stepFocus(emptyFocusHistory, "back").point).toBeUndefined();
  });
});
