import { useEffect, useRef, useState } from "react";
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

/**
 * The sanitized HTML for a Markdown source, published on a delay.
 *
 * The first render is deliberately not delayed: opening a document must paint
 * its preview, and there is nothing to debounce yet.
 */
export function useSanitizedMarkdown(source: string): string {
  const [published, setPublished] = useState(() => renderSafeMarkdown(source));
  const rendered = useRef(source);
  useEffect(() => {
    if (rendered.current === source) return;
    return scheduleSanitizedPreview(() => {
      rendered.current = source;
      setPublished(renderSafeMarkdown(source));
    });
  }, [source]);
  return published;
}
