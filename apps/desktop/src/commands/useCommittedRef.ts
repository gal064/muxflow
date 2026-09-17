import { useLayoutEffect, useRef } from "react";

/**
 * A stable ref holding the latest value React has actually committed.
 *
 * The problem it solves appears wherever something outside the render tree —
 * the command palette, a memoized row — needs the current handler without that
 * handler's identity churning its own dependents on every render. The ref is
 * stable; what it points at is refreshed once the render it came from is real.
 *
 * The commit is a layout effect, deliberately, and both halves of that matter:
 *
 * - Not during render. A render React discards still runs its body, so a
 *   render-time write leaves the ref pointing at a closure over props and
 *   state that were never committed — and the next caller acts on them.
 * - Not a passive effect. Passive effects flush after paint, leaving a window
 *   in which the ref still holds the previous commit's closure while the UI on
 *   screen is already the new one.
 *
 * Three sites had grown three different answers to this, one of them the
 * render-time write that a sibling file's comment already named as wrong.
 */
export function useCommittedRef<T>(value: T): { readonly current: T } {
  const committed = useRef(value);
  useLayoutEffect(() => {
    committed.current = value;
  });
  return committed;
}
