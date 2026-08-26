// Loading one directory for the Files screens (design doc §9.6).
//
// A "visit" is a focus: §9.6 says the listing is fetched once per visit, and a
// DIRECTORY_SNAPSHOT or FILE_CHANGED for the displayed directory re-lists
// silently. Both are wired here so the two screens (`index` and `dir`) share
// one implementation.

import { useCallback, useEffect, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";
import { useStore } from "zustand";
import { getConnection } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";
import { resolveRoot, rooted, type ResolvedRoot } from "./activeRoot";
import { displayMessageFor } from "./errors";
import { filesStore } from "./filesStore";
import { fetchDirectory, type DirectoryListing } from "./listing";

export type DirectoryView =
  /** Resolving the root, or waiting for the connection: §9.6 shows a centred spinner. */
  | { status: "loading"; root?: ResolvedRoot | undefined }
  | { status: "ready"; root: ResolvedRoot; listing: DirectoryListing }
  | { status: "error"; message: string; root?: ResolvedRoot | undefined };

export interface DirectoryResult {
  view: DirectoryView;
  /** The `Retry` button in §9.6 step 7. */
  reload: () => void;
}

/**
 * @param path the directory to list, or `undefined` for the pane's active root.
 */
export function useDirectory(paneId: string, path?: string | undefined): DirectoryResult {
  const [view, setView] = useState<DirectoryView>({ status: "loading" });
  const connectionState = useStore(sessionStore, (state) => state.connection.state);
  const revision = useStore(filesStore, (state) => (path === undefined ? 0 : (state.directoryRevisions[path] ?? 0)));
  const attempt = useRef(0);
  const focused = useRef(false);

  const load = useCallback(async () => {
    const token = ++attempt.current;
    const connection = getConnection();
    if (!connection || connectionState !== "connected") {
      // The global connection strip (§9) says why; this screen just waits.
      setView((previous) => (previous.status === "loading" ? previous : { status: "loading" }));
      return;
    }
    const request = connection.request.bind(connection);
    const identity = connection.serverIdentity;
    try {
      const known = filesStore.getState().roots[paneId];
      const root = await resolveRoot(request, paneId, identity, known);
      if (token !== attempt.current) return;
      filesStore.getState().setRoot(root);
      const listing = await fetchDirectory(request, rooted(root, path ?? root.root), identity);
      if (token !== attempt.current) return;
      setView({ status: "ready", root, listing });
    } catch (error) {
      if (token !== attempt.current) return;
      setView({ status: "error", message: displayMessageFor(error) });
    }
  }, [connectionState, paneId, path]);

  // Focus is the visit. `load` changes identity when the pane, the path or the
  // connection state changes, and a focused screen re-runs it then too.
  useFocusEffect(
    useCallback(() => {
      focused.current = true;
      void load();
      return () => {
        focused.current = false;
      };
    }, [load]),
  );

  // A host-driven change to this directory re-lists without a spinner.
  useEffect(() => {
    if (revision > 0 && focused.current) void load();
  }, [revision, load]);

  const reload = useCallback(() => {
    setView({ status: "loading" });
    void load();
  }, [load]);

  return { view, reload };
}
