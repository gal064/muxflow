// Loading one directory for the Files screens (design doc §9.6).
//
// A "visit" is a focus: §9.6 says the listing is fetched once per visit, and a
// DIRECTORY_SNAPSHOT or FILE_CHANGED for the displayed directory re-lists
// silently, as does an ACTIVE_ROOT that moves the pane's root (§11.2). All of
// them are wired here so the two screens (`index` and `dir`) share one
// implementation.

import { useCallback, useEffect, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";
import { useStore } from "zustand";
import { getConnection } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";
import { resolveRoot, rooted, type ResolvedRoot } from "./activeRoot";
import { displayMessageFor } from "./errors";
import { filesStore } from "./filesStore";
import { fetchDirectory, type DirectoryListing } from "./listing";
import { FILES_COPY } from "./presentation";

export type DirectoryView =
  /** Resolving the root, or waiting for the connection: §9.6 shows a centred spinner. */
  | { status: "loading" }
  | { status: "ready"; root: ResolvedRoot; listing: DirectoryListing }
  /** §9.6 step 7. The root the screen last knew stays in the store, for the title. */
  | { status: "error"; message: string };

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
  // The root screen only learns its directory once the root resolves, so the
  // revision is watched on whatever path was actually listed.
  const [listedPath, setListedPath] = useState(path);
  const revision = useStore(filesStore, (state) => (listedPath === undefined ? 0 : (state.directoryRevisions[listedPath] ?? 0)));
  const rootToken = useStore(filesStore, (state) => state.roots[paneId]?.rootToken ?? "");
  const attempt = useRef(0);
  const focused = useRef(false);
  /**
   * What the host had told us about this directory when it was last listed.
   * Seeded from the mount-time values so the first focus is the only load, and
   * refreshed by `load` so a listing this hook itself caused never re-triggers.
   */
  const applied = useRef(`${rootToken}:${revision}`);

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
      setListedPath(listing.path);
      applied.current = signature(paneId, listing.path);
      setView({ status: "ready", root, listing });
    } catch (error) {
      if (token !== attempt.current) return;
      setView({ status: "error", message: FILES_COPY.error(displayMessageFor(error)) });
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

  // A host-driven change — this directory's contents, or the pane's root —
  // re-lists without a spinner. Anything this hook did itself is already in
  // `applied`, so only the host's own news gets here.
  useEffect(() => {
    const current = `${rootToken}:${revision}`;
    if (applied.current === current) return;
    applied.current = current;
    if (focused.current) void load();
  }, [revision, rootToken, load]);

  const reload = useCallback(() => {
    setView({ status: "loading" });
    void load();
  }, [load]);

  return { view, reload };
}

function signature(paneId: string, listedPath: string): string {
  const state = filesStore.getState();
  return `${state.roots[paneId]?.rootToken ?? ""}:${state.directoryRevisions[listedPath] ?? 0}`;
}
