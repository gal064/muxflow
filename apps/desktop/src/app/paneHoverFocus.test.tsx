// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import type { PointerEvent } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DOCK_HOVER_TARGET, HOVER_FOCUS_DELAY_MS, usePaneHoverFocus, type PaneHoverFocusOptions, type PaneHoverHandlers } from "./paneHoverFocus";

const enter = (buttons = 0) => ({ buttons } as PointerEvent<HTMLElement>);

function harness(initial: Omit<PaneHoverFocusOptions, "focus">) {
  const focus = vi.fn();
  let handlers!: (paneId: string) => PaneHoverHandlers;
  function Harness(props: Omit<PaneHoverFocusOptions, "focus">) {
    handlers = usePaneHoverFocus({ ...props, focus });
    return null;
  }
  let renderer!: ReturnType<typeof create>;
  act(() => { renderer = create(<Harness {...initial} />); });
  return {
    focus,
    pane: (paneId: string) => handlers(paneId),
    rerender: (next: Omit<PaneHoverFocusOptions, "focus">) => act(() => renderer.update(<Harness {...next} />)),
  };
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
  document.body.replaceChildren();
});

describe("usePaneHoverFocus", () => {
  it("focuses a pane the pointer rests on", () => {
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS - 1);
    expect(focus).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(focus).toHaveBeenCalledWith("%2");
  });

  it("does nothing for a pass across, a held button, or a press", () => {
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    pane("%2").onPointerLeave();
    pane("%2").onPointerEnter(enter(1));
    pane("%3").onPointerEnter(enter());
    pane("%3").onPointerDown();
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS * 2);
    expect(focus).not.toHaveBeenCalled();
  });

  it("does nothing when off, when the pane is already active, or when it would steal focus from a field", () => {
    const { focus, pane, rerender } = harness({ enabled: false, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);

    rerender({ enabled: true, activePaneId: "%2" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);

    const search = document.createElement("input");
    document.body.append(search);
    search.focus();
    rerender({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });

  it("takes focus from a button, as closing Settings leaves it on the gear", () => {
    const gear = document.createElement("button");
    document.body.append(gear);
    gear.focus();
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).toHaveBeenCalledWith("%2");
  });

  it("takes focus from another terminal", () => {
    const terminal = document.createElement("div");
    terminal.dataset.terminalSurface = "true";
    const textarea = document.createElement("textarea");
    terminal.append(textarea);
    document.body.append(terminal);
    textarea.focus();
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).toHaveBeenCalledWith("%2");
  });

  it("moves between the panes and a docked document, whose editor is not a field to protect", () => {
    const docked = document.createElement("div");
    docked.dataset.dockedTab = "tab-1";
    const editor = document.createElement("textarea");
    editor.className = "inputarea";
    const monaco = document.createElement("div");
    monaco.className = "monaco-editor";
    monaco.append(editor);
    docked.append(monaco);
    document.body.append(docked);
    let inDock = false;
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1", dockHoldsFocus: () => inDock });

    pane(DOCK_HOVER_TARGET).onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).toHaveBeenLastCalledWith(DOCK_HOVER_TARGET);

    // With the caret in the docked editor, even the active pane takes it back.
    editor.focus();
    inDock = true;
    pane("%1").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).toHaveBeenLastCalledWith("%1");

    // Resting on the docked document while it has the keyboard does nothing.
    focus.mockClear();
    pane(DOCK_HOVER_TARGET).onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });

  it("keeps focus in the docked editor's Find field when the pointer rests on a terminal", () => {
    const docked = document.createElement("div");
    docked.dataset.dockedTab = "tab-1";
    const monaco = document.createElement("div");
    monaco.className = "monaco-editor";
    const search = document.createElement("input");
    monaco.append(search);
    docked.append(monaco);
    document.body.append(docked);
    search.focus();
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1", dockHoldsFocus: () => true });
    pane("%1").onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });

  it("does not take the docked document from a field outside it", () => {
    const search = document.createElement("input");
    document.body.append(search);
    search.focus();
    const { focus, pane } = harness({ enabled: true, activePaneId: "%1", dockHoldsFocus: () => false });
    pane(DOCK_HOVER_TARGET).onPointerEnter(enter());
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });

  it("cancels a pending focus when the setting turns off, as a dialog opening does", () => {
    const { focus, pane, rerender } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    rerender({ enabled: false, activePaneId: "%1" });
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });
});
