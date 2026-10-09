import { useEffect, useRef, useState } from "react";
import { useCommittedRef } from "../../commands/useCommittedRef";
import { keyForScope } from "../files/api";
import type { ActiveRoot, FileSearchClient, FileSearchResults, FileWorkspaceScope, TerminalFilePaneRoute } from "../files/types";

const EMPTY: FileSearchResults = { matches: [], complete: true };
interface State extends FileSearchResults { key: string; query: string; loading: boolean; error?: string }

/** One picker lifetime owns the root and its cancellable, debounced search. */
export function useQuickOpenSearch(client: FileSearchClient, scope: FileWorkspaceScope | undefined, pane: TerminalFilePaneRoute | undefined, query: string) {
  const key = scope && pane ? `${keyForScope(scope)}\0${pane.windowId}\0${pane.cwd}` : "";
  const latest = useCommittedRef({ scope, pane });
  const root = useRef<{ key: string; value: ActiveRoot } | undefined>(undefined);
  const [state, setState] = useState<State>({ ...EMPTY, key: "", query: "", loading: false });

  useEffect(() => {
    const captured = latest.current;
    if (!query || !key || !captured.scope || !captured.pane) {
      setState({ ...EMPTY, key, query, loading: false });
      return;
    }
    let live = true;
    const abort = new AbortController();
    setState({ ...EMPTY, key, query, loading: true });
    const timer = window.setTimeout(() => {
      void (async () => {
        try {
          const active = root.current?.key === key ? root.current.value
            : await client.resolveActiveRoot(captured.scope!, { signal: abort.signal });
          if (!live) return;
          root.current = { key, value: active };
          const results = await client.searchFiles(captured.scope!, active, captured.pane!, query, abort.signal);
          if (live) setState({ ...results, key, query, loading: false });
        } catch (error) {
          if (live && !abort.signal.aborted) {
            root.current = undefined;
            setState({ ...EMPTY, key, query, loading: false, error: String(error) });
          }
        }
      })();
    }, 100);
    return () => { live = false; window.clearTimeout(timer); abort.abort(); };
  }, [client, key, latest, query]);

  return state.key === key && state.query === query ? state : {
    ...EMPTY, key, query, loading: Boolean(query && key),
  };
}
