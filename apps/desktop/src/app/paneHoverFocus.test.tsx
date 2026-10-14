// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import type { PointerEvent } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HOVER_FOCUS_DELAY_MS, usePaneHoverFocus, type PaneHoverFocusOptions, type PaneHoverHandlers } from "./paneHoverFocus";

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

  it("cancels a pending focus when the setting turns off, as a dialog opening does", () => {
    const { focus, pane, rerender } = harness({ enabled: true, activePaneId: "%1" });
    pane("%2").onPointerEnter(enter());
    rerender({ enabled: false, activePaneId: "%1" });
    vi.advanceTimersByTime(HOVER_FOCUS_DELAY_MS);
    expect(focus).not.toHaveBeenCalled();
  });
});
