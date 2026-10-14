// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PointerEvent as ReactPointerEvent } from "react";
import {
  beginPointerDrag, currentDragState, paneEdgeAt, pointerDragActive, setDropResolver, type DragPayload,
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
