/**
 * The mounted pane renderers, so something outside the terminal feature can
 * read what a pane is showing.
 *
 * The AI assistant's `read_pane` tool is the only consumer today: it reads
 * the same screen the user is looking at, from the xterm buffer that already
 * exists, rather than asking the host for another capture. Only *mounted*
 * panes are here — the current tab's grid — which is exactly the set the
 * assistant's tools are scoped to; a hidden pane's screen lives in the
 * serialized state cache, not in a buffer anyone could read as text.
 *
 * Keyed by `terminalCacheKey(scope, paneId)`, the same key the screen cache
 * uses: pane ids repeat across tmux servers, and `%1` on one host must not
 * answer for `%1` on another.
 */
const readers = new Map<string, (maxRows: number) => string>();

/**
 * Publishes one mounted renderer's screen. Returns the unregister the mount's
 * cleanup must call; registering the same key twice replaces the older reader,
 * which is what a remount that raced its own cleanup gets away with.
 */
export function registerPaneScreenReader(
  cacheKey: string,
  read: (maxRows: number) => string,
): () => void {
  readers.set(cacheKey, read);
  return () => {
    if (readers.get(cacheKey) === read) readers.delete(cacheKey);
  };
}

/** The pane's screen as plain text, or `undefined` when no renderer holds it. */
export function readPaneScreen(cacheKey: string, maxRows: number): string | undefined {
  return readers.get(cacheKey)?.(maxRows);
}
