import { describe, expect, it } from "vitest";
import {
  selectionEdgeScrollDirection,
  selectionForTerminalRange,
  TerminalTouchIntent,
  TERMINAL_TOUCH_SLOP_PX,
} from "./touchIntent";

describe("terminal touch intent", () => {
  it("keeps a short stationary gesture as a tap", () => {
    const intent = new TerminalTouchIntent();
    intent.start({ x: 10, y: 20 });
    expect(intent.move({ x: 10 + TERMINAL_TOUCH_SLOP_PX, y: 20 })).toBe("pending");
    expect(intent.end()).toBe("tap");
  });

  it("commits to scrolling after the movement threshold", () => {
    const intent = new TerminalTouchIntent();
    intent.start({ x: 10, y: 20 });
    expect(intent.move({ x: 19, y: 20 })).toBe("startScroll");
    expect(intent.move({ x: 22, y: 20 })).toBe("scroll");
    expect(intent.longPress()).toBe(false);
    expect(intent.end()).toBe("scroll");
  });

  it("commits a pending gesture to selection and never turns it into scrolling", () => {
    const intent = new TerminalTouchIntent();
    intent.start({ x: 10, y: 20 });
    expect(intent.longPress()).toBe(true);
    expect(intent.move({ x: 40, y: 80 })).toBe("selection");
    expect(intent.end()).toBe("selection");
  });
});

describe("terminal selection geometry", () => {
  it("converts a wrapped inclusive link range to xterm selection arguments", () => {
    expect(selectionForTerminalRange({
      start: { x: 7, y: 3 },
      end: { x: 5, y: 4 },
    }, 20)).toEqual({ column: 6, row: 2, length: 19 });
  });

  it("classifies only the top and bottom edge zones for continuous scrolling", () => {
    expect(selectionEdgeScrollDirection(119, 100, 500, 20)).toBe(-1);
    expect(selectionEdgeScrollDirection(120, 100, 500, 20)).toBe(0);
    expect(selectionEdgeScrollDirection(480, 100, 500, 20)).toBe(0);
    expect(selectionEdgeScrollDirection(481, 100, 500, 20)).toBe(1);
  });
});
