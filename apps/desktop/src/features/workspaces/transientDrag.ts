import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";

type PointerGeometry = Pick<globalThis.PointerEvent, "clientX" | "clientY">;
type ValueAtPointer = (event: PointerGeometry) => number;

/**
 * Owns one complete captured-pointer gesture: rAF preview, pointer identity,
 * final persistence, lost-capture handling, listener teardown, and unmount.
 */
export function useTransientDrag(
  committed: number,
  persist: (value: number) => void,
): readonly [number, (event: ReactPointerEvent<HTMLElement>, valueAt: ValueAtPointer) => void] {
  const [draft, setDraft] = useState(committed);
  const active = useRef<(() => void) | undefined>(undefined);
  const committedRef = useRef(committed);
  const persistRef = useRef(persist);
  committedRef.current = committed;
  persistRef.current = persist;

  useEffect(() => {
    if (!active.current) setDraft(committed);
  }, [committed]);
  useEffect(() => () => active.current?.(), []);

  const start = useCallback((event: ReactPointerEvent<HTMLElement>, valueAt: ValueAtPointer) => {
    // A captured gesture has one owner. Foreign/second pointers do not replace
    // it or inherit its final commit.
    if (active.current) return;
    const target = event.currentTarget;
    const pointerId = event.pointerId;
    let latest = committedRef.current;
    let frame: number | undefined;
    let closed = false;

    const preview = (pointer: globalThis.PointerEvent) => {
      if (pointer.pointerId !== pointerId || closed) return;
      latest = valueAt(pointer);
      if (frame !== undefined) return;
      frame = window.requestAnimationFrame(() => {
        frame = undefined;
        if (!closed) setDraft(latest);
      });
    };
    const teardown = (commit: boolean, pointer?: globalThis.PointerEvent) => {
      if (closed || (pointer && pointer.pointerId !== pointerId)) return;
      if (pointer?.type === "pointerup") latest = valueAt(pointer);
      closed = true;
      if (frame !== undefined) window.cancelAnimationFrame(frame);
      target.removeEventListener("pointermove", preview);
      target.removeEventListener("pointerup", finish);
      target.removeEventListener("pointercancel", finish);
      target.removeEventListener("lostpointercapture", finish);
      active.current = undefined;
      if (target.hasPointerCapture?.(pointerId)) target.releasePointerCapture(pointerId);
      if (commit) {
        setDraft(latest);
        persistRef.current(latest);
      }
    };
    const finish = (pointer: globalThis.PointerEvent) => teardown(true, pointer);
    active.current = () => teardown(false);
    target.addEventListener("pointermove", preview);
    target.addEventListener("pointerup", finish);
    target.addEventListener("pointercancel", finish);
    target.addEventListener("lostpointercapture", finish);
    target.setPointerCapture(pointerId);
  }, []);

  return [draft, start] as const;
}
