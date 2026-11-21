import { createContext, useCallback, useContext, useLayoutEffect, useRef, useSyncExternalStore, type RefObject } from "react";
import type { View } from "react-native";

// What the reader has opened in the conversation: steps lines, thoughts and
// tool rows. It is kept by the conversation and not by the rows, because the
// list lets go of a row that scrolls far away and draws it afresh when it
// comes back, and a line the reader opened is still open then.

/** The lines of one kind that are open, by key. A line reads its own key and is drawn again only when that one changes. */
export class OpenKeys {
  private readonly open = new Set<string>();
  private readonly listeners = new Map<string, Set<() => void>>();

  has(key: string): boolean {
    return this.open.has(key);
  }

  toggle(key: string): void {
    if (!this.open.delete(key)) this.open.add(key);
    for (const listener of this.listeners.get(key) ?? []) listener();
  }

  subscribe(key: string, listener: () => void): () => void {
    let listeners = this.listeners.get(key);
    if (!listeners) this.listeners.set(key, (listeners = new Set()));
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) this.listeners.delete(key);
    };
  }
}

export interface TimelineOpen {
  /** Steps lines, by their row's key. */
  groups: OpenKeys;
  /** Thoughts and tool rows, by their item's id. */
  rows: OpenKeys;
  /**
   * The reader is about to open or close something in the row `key`, drawn in
   * `cell`. Answers what to call once the change is drawn; the conversation
   * then keeps the tapped line where it was on screen.
   */
  hold: (key: string, cell: View | null) => () => void;
  /** The row `key` is laid out `height` tall. */
  laidOut: (key: string, height: number) => void;
}

/** Outside a conversation nothing is kept and nothing needs holding. */
export const TimelineOpenContext = createContext<TimelineOpen>({ groups: new OpenKeys(), rows: new OpenKeys(), hold: () => () => undefined, laidOut: () => undefined });

/** The list row something is drawn in. */
export interface TimelineCell {
  key: string;
  view: RefObject<View | null>;
}

export const TimelineCellContext = createContext<TimelineCell>({ key: "", view: { current: null } });

/**
 * Whether the line `id` is open, and what a tap on it calls. The row it is in
 * changes height when it opens or closes; the conversation is told before and
 * after, so the line that was tapped does not move.
 */
export function useHeldOpen(kind: "groups" | "rows", id: string): [open: boolean, toggle: () => void] {
  const timeline = useContext(TimelineOpenContext);
  const cell = useContext(TimelineCellContext);
  const keys = timeline[kind];
  const open = useSyncExternalStore(
    useCallback((changed: () => void) => keys.subscribe(id, changed), [id, keys]),
    () => keys.has(id),
  );
  const drawn = useRef<(() => void) | undefined>(undefined);
  const toggle = useCallback(() => {
    drawn.current = timeline.hold(cell.key, cell.view.current);
    keys.toggle(id);
  }, [cell, id, keys, timeline]);
  // After the row is laid out at its new height, and before that is on screen.
  useLayoutEffect(() => {
    const done = drawn.current;
    drawn.current = undefined;
    done?.();
  }, [open]);
  return [open, toggle];
}
