import { describe, expect, it } from "vitest";
import {
  canGoBack,
  canGoForward,
  emptyFocusHistory,
  FOCUS_HISTORY_LIMIT,
  hasValidStep,
  nearestValidBack,
  pruneFocusHistory,
  stepFocus,
  stepFocusToValid,
  visitFocus,
} from "./focusHistory";

const at = (sessionId: string, windowId?: string, appTabId?: string) => ({ sessionId, windowId, appTabId });

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

  it("tells a document tab apart from the terminal it was opened over", () => {
    let history = visitFocus(emptyFocusHistory, at("$1", "@1"));
    history = visitFocus(history, at("$1", "@1", "notes"));
    expect(history.entries).toHaveLength(2);
    expect(visitFocus(history, at("$1", "@1", "notes"))).toBe(history);
    expect(stepFocus(history, "back").point).toEqual(at("$1", "@1"));
  });

  it("steps over entries that no longer exist instead of stopping on them", () => {
    let history = emptyFocusHistory;
    for (const point of [at("$1", "@1"), at("$1", "@1", "gone"), at("$1", "@1", "diff"), at("$2", "@2")]) history = visitFocus(history, point);
    const exists = (point: { appTabId?: string }) => point.appTabId !== "gone" && point.appTabId !== "diff";
    expect(hasValidStep(history, "back", exists)).toBe(true);
    expect(hasValidStep(history, "forward", exists)).toBe(false);
    const back = stepFocusToValid(history, "back", exists);
    expect(back.point).toEqual(at("$1", "@1"));
    expect(back.history.cursor).toBe(0);
    expect(hasValidStep(back.history, "forward", exists)).toBe(true);
    expect(stepFocusToValid(back.history, "forward", exists).point).toEqual(at("$2", "@2"));
    // Nothing valid in that direction leaves the history where it was.
    expect(stepFocusToValid(back.history, "back", exists)).toEqual({ history: back.history });
    expect(hasValidStep(history, "back", () => false)).toBe(false);
  });

  it("finds where a closing tab should hand focus, never to itself", () => {
    let history = emptyFocusHistory;
    for (const point of [at("$1", "@1"), at("$1", "@1", "notes"), at("$1", "@2"), at("$1", "@2", "notes")]) history = visitFocus(history, point);
    expect(nearestValidBack(history, () => true, { appTabId: "notes" })).toEqual({ index: 2, point: at("$1", "@2") });
    expect(nearestValidBack(history, (point) => point.windowId !== "@2", { appTabId: "notes" })).toEqual({ index: 0, point: at("$1", "@1") });
    expect(nearestValidBack(history, () => false, { appTabId: "notes" })).toBeUndefined();
    expect(nearestValidBack(visitFocus(emptyFocusHistory, at("$1", "@1", "notes")), () => true, { appTabId: "notes" })).toBeUndefined();
  });

  it("has nowhere to go when it is empty", () => {
    expect(canGoBack(emptyFocusHistory)).toBe(false);
    expect(canGoForward(emptyFocusHistory)).toBe(false);
    expect(stepFocus(emptyFocusHistory, "back").point).toBeUndefined();
  });
});
