// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useTransientDrag } from "./transientDrag";

class PointerTarget {
  listeners = new Map<string, Set<(event: globalThis.PointerEvent) => void>>();
  captured?: number;
  addEventListener(type: string, listener: (event: globalThis.PointerEvent) => void) {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }
  removeEventListener(type: string, listener: (event: globalThis.PointerEvent) => void) {
    this.listeners.get(type)?.delete(listener);
  }
  setPointerCapture(pointerId: number) { this.captured = pointerId; }
  hasPointerCapture(pointerId: number) { return this.captured === pointerId; }
  releasePointerCapture(pointerId: number) { if (this.captured === pointerId) this.captured = undefined; }
  emit(type: string, pointerId: number, clientX: number) {
    const event = { type, pointerId, clientX, clientY: 0 } as globalThis.PointerEvent;
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event);
  }
}

describe("transient sidebar drag", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  });

  it("owns pointer identity, coalesces previews, and persists exactly once on pointer-up", async () => {
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrame = 0;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.set(++nextFrame, callback);
      return nextFrame;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation((handle) => { frames.delete(handle); });
    const persist = vi.fn();
    const target = new PointerTarget();
    let start!: ReturnType<typeof useTransientDrag>[1];
    function Harness() {
      [, start] = useTransientDrag(240, persist);
      return null;
    }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    const down = (pointerId: number) => ({ currentTarget: target, pointerId } as unknown as React.PointerEvent<HTMLElement>);
    act(() => start(down(1), (event) => event.clientX));
    act(() => start(down(2), (event) => event.clientX));
    act(() => {
      target.emit("pointermove", 1, 250);
      target.emit("pointermove", 1, 260);
    });
    expect(frames).toHaveLength(1);
    act(() => target.emit("pointerup", 2, 999));
    expect(persist).not.toHaveBeenCalled();
    act(() => {
      target.emit("pointerup", 1, 280);
      target.emit("lostpointercapture", 1, 280);
    });
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith(280);
    expect(target.captured).toBeUndefined();
    await act(async () => renderer.unmount());
  });

  it.each(["pointercancel", "lostpointercapture"])("commits the last preview once on %s", async (ending) => {
    vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 1);
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => undefined);
    const persist = vi.fn();
    const target = new PointerTarget();
    let start!: ReturnType<typeof useTransientDrag>[1];
    function Harness() { [, start] = useTransientDrag(0.4, persist); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    act(() => start({ currentTarget: target, pointerId: 7 } as unknown as React.PointerEvent<HTMLElement>, (event) => event.clientX));
    act(() => {
      target.emit("pointermove", 7, 0.6);
      target.emit(ending, 7, 0.9);
      target.emit(ending, 7, 1);
    });
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith(0.6);
    await act(async () => renderer.unmount());
  });

  it("removes native listeners without persisting on unmount", async () => {
    const persist = vi.fn();
    const target = new PointerTarget();
    let start!: ReturnType<typeof useTransientDrag>[1];
    function Harness() { [, start] = useTransientDrag(240, persist); return null; }
    let renderer!: ReturnType<typeof create>;
    await act(async () => { renderer = create(<Harness />); });
    act(() => start({ currentTarget: target, pointerId: 1 } as unknown as React.PointerEvent<HTMLElement>, (event) => event.clientX));
    await act(async () => renderer.unmount());
    expect([...target.listeners.values()].every((listeners) => listeners.size === 0)).toBe(true);
    expect(persist).not.toHaveBeenCalled();
  });
});
