/**
 * How long the preload may wait for a quiet moment before going anyway.
 *
 * The whole point of this preload is that it costs the user nothing, so it
 * wants a genuinely idle page rather than the next frame. But a shell that
 * never goes idle — a terminal streaming output from the moment it connects is
 * the ordinary case, not the exotic one — would otherwise never preload at all,
 * and the first file open would pay the full fetch exactly as it does today.
 * The deadline is what makes this a "later" rather than a "maybe".
 */
export const EDITOR_PRELOAD_TIMEOUT_MS = 3_000;

/**
 * Fetches and evaluates the editor chunk, off the path that needs it.
 *
 * The editor is a 3.9 MB chunk that today loads strictly *after* a file's
 * content has arrived: `AppTabSurface` only mounts `FileEditor` once there is
 * text to put in it, so the fetch, the eval and the mount are serialized behind
 * the read instead of overlapping it. Measured at ~490 ms on the first open of
 * a session, plus up to 100 ms of content gate, and paid once per launch
 * (tests/phase15/decomposition.md).
 *
 * This is deliberately a dynamic import and not a static one. A static import
 * would fold Monaco into the entry chunk — 1.0 MB to ~5 MB — which every
 * terminal-only launch would then download and evaluate before its first frame,
 * to the benefit of nobody who never opens a file. The dynamic import leaves
 * the chunk split exactly where the bundler already put it and changes only
 * *when* it is asked for; because the module registry caches, the `import()`
 * that `AppTabSurface` issues later resolves against this one instead of
 * fetching again.
 *
 * A failure is swallowed. Nothing here is load-bearing: if the chunk cannot be
 * fetched now, the lazy boundary in `AppTabSurface` will try again when a file
 * is actually opened and will render its own error state if that fails too.
 * Rejecting out of a fire-and-forget idle callback would only produce an
 * unhandled rejection for a fault the app already handles.
 */
export function preloadEditorChunk(): Promise<void> {
  return import("../features/files/FileEditor").then(
    () => undefined,
    (error: unknown) => {
      console.warn("editor preload failed; it will load on demand instead", error);
    },
  );
}

/**
 * Runs `work` once the page goes idle, or once the deadline passes.
 *
 * Same shape as `markdownPreview.ts`'s idle scheduling, including the fallback:
 * `requestIdleCallback` is absent in jsdom and in Safari's older engines, and
 * the caller should not have to know which one it is running on.
 */
function whenIdle(work: () => void): () => void {
  const request = globalThis.requestIdleCallback;
  if (typeof request !== "function") {
    const timer = setTimeout(work, 0);
    return () => clearTimeout(timer);
  }
  const handle = request(work, { timeout: EDITOR_PRELOAD_TIMEOUT_MS });
  return () => globalThis.cancelIdleCallback?.(handle);
}

/**
 * Schedules the editor preload for the next idle moment.
 *
 * Called from bootstrap rather than from any surface, and that placement is
 * load-bearing rather than incidental: `editorChunkAbsence.test.tsx` pins that
 * a surface showing content which cannot use an editor — a binary file, an
 * oversized file, a Markdown preview, a disconnected tab — never reaches the
 * editor bundle at all. A preload hung off a surface would break that claim for
 * every one of those cases. Bootstrap has no such claim to keep: it preloads
 * once per launch on behalf of whatever the session turns out to do.
 *
 * Returns a cancel so a caller that changes its mind — and the tests — can
 * withdraw the scheduled work.
 */
export function scheduleEditorPreload(): () => void {
  return whenIdle(() => {
    void preloadEditorChunk();
  });
}
