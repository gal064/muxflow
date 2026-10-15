// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PointerEvent as ReactPointerEvent } from "react";
import {
  beginPointerDrag, currentDragState, dropTargetAt, paneEdgeAt, pointerDragActive, setDropResolver, type DragPayload,
} from "./pointerDrag";

const payload: DragPayload = { kind: "terminalTab", hostProfileId: "local", sessionId: "$1", windowId: "@2", label: "two" };

function pointer(type: string, x: number, y: number, pointerId = 1): PointerEvent {
  // A move carries the held primary button; a release or cancel does not.
  const buttons = type === "pointermove" ? 1 : 0;
  const event = new MouseEvent(type, { bubbles: true, clientX: x, clientY: y, button: 0, buttons }) as unknown as PointerEvent;
  Object.defineProperty(event, "pointerId", { value: pointerId });
  return event;
}

/** A source button and a workspace row standing at every point. */
function setup() {
  const source = document.createElement("button");
  const row = document.createElement("div");
  row.dataset.dropWorkspace = "$2";
  row.dataset.dropHost = "local";
  document.body.append(source, row);
  document.elementsFromPoint = vi.fn(() => [row]);
  const press = { button: 0, pointerId: 1, clientX: 10, clientY: 10, currentTarget: source } as unknown as ReactPointerEvent<HTMLElement>;
  return { source, press };
}

afterEach(() => {
  window.dispatchEvent(pointer("pointercancel", 0, 0));
  setDropResolver(undefined);
  document.body.replaceChildren();
  vi.useRealTimers();
});

describe("paneEdgeAt", () => {
  const rect = { left: 0, top: 0, width: 100, height: 100 };
  it("picks the nearest edge and leaves the middle as no drop", () => {
    expect(paneEdgeAt(rect, 5, 50)).toBe("left");
    expect(paneEdgeAt(rect, 95, 50)).toBe("right");
    expect(paneEdgeAt(rect, 50, 4)).toBe("top");
    expect(paneEdgeAt(rect, 50, 90)).toBe("bottom");
    expect(paneEdgeAt(rect, 50, 50)).toBeUndefined();
  });
});

describe("focus after a drag", () => {
  it("goes back to where it was before the press, not the row the drag started on", () => {
    const { source, press } = setup();
    const terminal = document.createElement("textarea");
    document.body.append(terminal);
    terminal.focus();
    setDropResolver(() => ({ kind: "run", run: () => undefined }));
    beginPointerDrag(press, payload);
    source.focus(); // the press's own default
    window.dispatchEvent(pointer("pointermove", 30, 30));
    window.dispatchEvent(pointer("pointerup", 30, 30));
    expect(document.activeElement).toBe(terminal);
  });

  it("leaves a plain click's focus alone", () => {
    const { source, press } = setup();
    beginPointerDrag(press, payload);
    source.focus();
    window.dispatchEvent(pointer("pointerup", 10, 10));
    expect(document.activeElement).toBe(source);
  });
});

describe("dropTargetAt", () => {
  it("reads a tab inside the strip as that tab, carrying the strip's workspace", () => {
    const strip = document.createElement("div");
    strip.dataset.dropWorkspace = "$1";
    strip.dataset.dropHost = "local";
    const tab = document.createElement("div");
    tab.dataset.dropTab = "terminal:@2";
    tab.getBoundingClientRect = () => ({ left: 0, width: 100 }) as DOMRect;
    strip.append(tab);
    document.body.append(strip);
    document.elementsFromPoint = vi.fn(() => [tab, strip]);
    expect(dropTargetAt(80, 5)).toEqual({
      kind: "tab", tabKey: "terminal:@2", side: "after", workspace: { hostProfileId: "local", sessionId: "$1" },
    });
    // The strip's empty end is the workspace itself.
    document.elementsFromPoint = vi.fn(() => [strip]);
    expect(dropTargetAt(300, 5)).toEqual({ kind: "workspace", hostProfileId: "local", sessionId: "$1" });
  });

  it("reads a dock zone over the edge pane under it", () => {
    const pane = document.createElement("div");
    pane.dataset.dropPane = "%1";
    pane.getBoundingClientRect = () => ({ left: 0, top: 0, width: 100, height: 100 }) as DOMRect;
    const zone = document.createElement("div");
    zone.dataset.dropDock = "left";
    document.body.append(pane, zone);
    document.elementsFromPoint = vi.fn(() => [zone, pane]);
    expect(dropTargetAt(5, 50)).toEqual({ kind: "dock", side: "left" });
    // Without the zone (no drag that can dock), the same point is the pane's edge.
    document.elementsFromPoint = vi.fn(() => [pane]);
    expect(dropTargetAt(5, 50)).toEqual({ kind: "pane", paneId: "%1", edge: "left" });
  });
});

describe("beginPointerDrag", () => {
  it("stays a click until the pointer passes the threshold", () => {
    const { press } = setup();
    const run = vi.fn();
    setDropResolver(() => ({ kind: "run", run }));
    beginPointerDrag(press, payload);
    window.dispatchEvent(pointer("pointermove", 12, 11));
    expect(pointerDragActive()).toBe(false);
    window.dispatchEvent(pointer("pointerup", 12, 11));
    expect(run).not.toHaveBeenCalled();
  });

  it("runs the resolved drop on release and swallows the click that follows", () => {
    const { source, press } = setup();
    const run = vi.fn();
    setDropResolver(() => ({ kind: "run", run }));
    beginPointerDrag(press, payload);
    window.dispatchEvent(pointer("pointermove", 40, 40));
    expect(currentDragState()?.payload).toBe(payload);
    window.dispatchEvent(pointer("pointerup", 40, 40));
    expect(run).toHaveBeenCalledTimes(1);
    expect(pointerDragActive()).toBe(false);
    const clicked = vi.fn();
    source.addEventListener("click", clicked);
    source.click();
    expect(clicked).not.toHaveBeenCalled();
  });

  it("reports a refusal instead of running anything", () => {
    const { press } = setup();
    const refused = vi.fn();
    setDropResolver(() => ({ kind: "refuse", reason: "no" }), refused);
    beginPointerDrag(press, payload);
    window.dispatchEvent(pointer("pointermove", 40, 40));
    window.dispatchEvent(pointer("pointerup", 40, 40));
    expect(refused).toHaveBeenCalledWith("no");
  });

  it("drops nothing when Escape cancels the drag", () => {
    const { press } = setup();
    const run = vi.fn();
    setDropResolver(() => ({ kind: "run", run }));
    beginPointerDrag(press, payload);
    window.dispatchEvent(pointer("pointermove", 40, 40));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(pointerDragActive()).toBe(false);
    window.dispatchEvent(pointer("pointerup", 40, 40));
    expect(run).not.toHaveBeenCalled();
  });

  it("swallows the click that the release after an Escape produces", () => {
    vi.useFakeTimers();
    const { source, press } = setup();
    setDropResolver(() => ({ kind: "run", run: vi.fn() }));
    beginPointerDrag(press, payload);
    window.dispatchEvent(pointer("pointermove", 40, 40));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    // The user lets go well after pressing Escape.
    vi.advanceTimersByTime(500);
    window.dispatchEvent(pointer("pointerup", 40, 40));
    const clicked = vi.fn();
    source.addEventListener("click", clicked);
    source.click();
    expect(clicked).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    source.click();
    expect(clicked).toHaveBeenCalledTimes(1);
  });

  it("disarms when a move arrives with the button already up", () => {
    const { press } = setup();
    beginPointerDrag(press, payload);
    const unpressed = new MouseEvent("pointermove", { bubbles: true, clientX: 40, clientY: 40, buttons: 0 });
    Object.defineProperty(unpressed, "pointerId", { value: 1 });
    window.dispatchEvent(unpressed);
    window.dispatchEvent(pointer("pointermove", 80, 80));
    expect(pointerDragActive()).toBe(false);
  });

  it("ignores a press with nothing to carry", () => {
    const { press } = setup();
    beginPointerDrag(press, undefined);
    window.dispatchEvent(pointer("pointermove", 40, 40));
    expect(pointerDragActive()).toBe(false);
  });
});
