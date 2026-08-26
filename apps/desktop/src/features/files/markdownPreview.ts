import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { renderSafeMarkdown } from "./markdown";

/**
 * How long typing has to stop before the preview is re-sanitized.
 *
 * Parsing and sanitizing a document is not free — it is a full Markdown parse,
 * a DOMPurify pass and a `DOMParser` round trip, on the same thread as the
 * keystroke. Publishing it per keystroke made a long document's preview the
 * most expensive thing in the editor's input path, while nothing about the
 * editor or the save state ever waited for it. The buffer, the dirty flag and
 * autosave stay immediate; only the rendered article lags.
 */
export const PREVIEW_DEBOUNCE_MS = 120;

/** How long a quiet moment may be waited for before publishing anyway. */
export const PREVIEW_IDLE_TIMEOUT_MS = 400;

/**
 * Runs the sanitize once typing has paused and the page is idle.
 *
 * Two stages, because either alone is wrong: an idle callback during steady
 * typing fires in the gaps between keystrokes and debounces nothing, while a
 * plain timer publishes into the middle of whatever the frame was already
 * doing. The idle deadline is what stops a permanently busy page from never
 * showing the preview at all.
 */
export function scheduleSanitizedPreview(work: () => void): () => void {
  let cancelIdle: (() => void) | undefined;
  const timer = setTimeout(() => { cancelIdle = whenIdle(work); }, PREVIEW_DEBOUNCE_MS);
  return () => {
    clearTimeout(timer);
    cancelIdle?.();
  };
}

function whenIdle(work: () => void): () => void {
  const request = globalThis.requestIdleCallback;
  if (typeof request !== "function") {
    const timer = setTimeout(work, 0);
    return () => clearTimeout(timer);
  }
  const handle = request(work, { timeout: PREVIEW_IDLE_TIMEOUT_MS });
  return () => globalThis.cancelIdleCallback?.(handle);
}

/** How far the pointer may travel between press and release and still be a click. */
export const CLICK_SLOP_PX = 4;

interface Sanitized {
  source: string;
  html: string;
}

interface PreviewOptions {
  /**
   * A pointer selection gesture is in progress. Nothing may be published until
   * it ends, because replacing the subtree mid-drag collapses the selection the
   * drag is building.
   */
  held?: boolean;
  /** The article the HTML is rendered into, used to locate a live selection. */
  container?: RefObject<HTMLElement | null>;
}

/**
 * True while a selection the user can see covers text inside `container`.
 *
 * Publishing replaces the whole subtree, which drops or moves any selection
 * living in it. A collapsed caret has nothing to lose and a selection somewhere
 * else in the app is not ours to protect, so only this case defers.
 *
 * Both tests are needed. An endpoint inside the article is the ordinary drag,
 * including one begun in the editor pane and dragged in. But `contains` only
 * looks downwards, so a select-all — both endpoints on the body, an ancestor —
 * reads as outside; a range that merely spans the article covers every word in
 * it and is the selection with the most to lose.
 */
export function selectionHolds(container: HTMLElement | null | undefined): boolean {
  if (!container) return false;
  const selection = document.getSelection?.();
  if (!selection || selection.isCollapsed) return false;
  if ([selection.anchorNode, selection.focusNode].some((node) => Boolean(node && container.contains(node)))) return true;
  for (let index = 0; index < selection.rangeCount; index += 1) {
    if (selection.getRangeAt(index).intersectsNode?.(container)) return true;
  }
  return false;
}

/**
 * The sanitized HTML for a Markdown source, published on a delay.
 *
 * The first render is deliberately not delayed: opening a document must paint
 * its preview, and there is nothing to debounce yet.
 *
 * A finished sanitize is parked rather than published while the user is
 * selecting text in the preview: an autosave-driven re-render arriving in the
 * middle of a drag would replace the nodes the selection points at, and the
 * selection would collapse. The park is released — once — as soon as the
 * gesture ends and the selection is gone or has moved out of the article, so a
 * live selection defers the refresh instead of freezing the preview.
 */
export function useSanitizedMarkdown(source: string, options?: PreviewOptions): string {
  const [published, setPublished] = useState(() => renderSafeMarkdown(source));
  // The published string is read from inside scheduled work and from listeners,
  // so it is mirrored here rather than closed over at whatever render armed them.
  const publishedRef = useRef(published);
  const rendered = useRef(source);
  const [parked, setParked] = useState<Sanitized>();
  const held = options?.held ?? false;
  const container = options?.container;

  // Read from inside scheduled work, which outlives the render that armed it.
  const heldRef = useRef(held);
  useEffect(() => { heldRef.current = held; }, [held]);

  const publish = useCallback((next: Sanitized) => {
    rendered.current = next.source;
    setParked(undefined);
    // Identical HTML is not a new document. Skipping the state write keeps
    // React from touching the subtree at all, selection or no selection.
    if (publishedRef.current === next.html) return;
    publishedRef.current = next.html;
    setPublished(next.html);
  }, []);

  useEffect(() => {
    // Back to what is already on screen — an undo, or an agent restoring a file
    // it had rewritten. Anything parked describes a document that no longer
    // exists, and publishing it later would leave the preview stuck showing it.
    if (rendered.current === source) { setParked(undefined); return; }
    return scheduleSanitizedPreview(() => {
      const next = { source, html: renderSafeMarkdown(source) };
      // `held` is read through the render that armed this schedule, which is
      // the render the gesture started in or a later one; a gesture that began
      // after the schedule is caught by the selection test below or, before any
      // text is selected, by the pointer-up release publishing the park.
      if (heldRef.current || selectionHolds(container?.current)) {
        setParked(next);
        return;
      }
      publish(next);
    });
  }, [container, publish, source]);

  useEffect(() => {
    if (!parked || held) return;
    if (!selectionHolds(container?.current)) {
      publish(parked);
      return;
    }
    // Still selected. Wait for the selection to collapse or leave rather than
    // dropping the update, and stop listening the moment it is published.
    const recheck = () => {
      if (selectionHolds(container?.current)) return;
      document.removeEventListener("selectionchange", recheck);
      publish(parked);
    };
    document.addEventListener("selectionchange", recheck);
    return () => document.removeEventListener("selectionchange", recheck);
  }, [container, held, parked, publish]);

  return published;
}
