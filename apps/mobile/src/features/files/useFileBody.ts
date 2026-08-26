// Loading one file for the viewer (design doc §9.7, §11.1).
//
// The root capability comes from the Files screen that pushed this one; the
// body comes from the **bulk** connection, which is the only lane the host
// serves file bodies on.

import { useCallback, useEffect, useRef, useState } from "react";
import { useStore } from "zustand";
import { getConnection, openBulkConnection } from "../../session/connectionManager";
import { sessionStore } from "../../store/sessionStore";
import { resolveRoot, rooted, type ResolvedRoot } from "./activeRoot";
import { displayMessageFor } from "./errors";
import { readFile } from "./fileStream";
import { filesStore } from "./filesStore";
import { filePresentation, FILE_VIEWER_COPY, type FilePresentation } from "./presentation";

export type FileView =
  | { status: "loading" }
  | { status: "ready"; root: ResolvedRoot; presentation: FilePresentation }
  | { status: "error"; message: string };

export interface FileBodyResult {
  view: FileView;
  /** The `Retry` button in §9.7 step 3. */
  reload: () => void;
}

export function useFileBody(paneId: string, path: string, name: string): FileBodyResult {
  const [view, setView] = useState<FileView>({ status: "loading" });
  const connectionState = useStore(sessionStore, (state) => state.connection.state);
  const attempt = useRef(0);

  const load = useCallback(async () => {
    const token = ++attempt.current;
    const connection = getConnection();
    if (!connection || connectionState !== "connected") {
      setView((previous) => (previous.status === "loading" ? previous : { status: "loading" }));
      return;
    }
    const identity = connection.serverIdentity;
    try {
      const known = filesStore.getState().roots[paneId];
      const root = await resolveRoot(connection.request.bind(connection), paneId, identity, known);
      if (token !== attempt.current) return;
      filesStore.getState().setRoot(root);
      const bulk = await openBulkConnection();
      if (token !== attempt.current) return;
      const body = await readFile(bulk.request.bind(bulk), rooted(root, path), identity);
      if (token !== attempt.current) return;
      setView({ status: "ready", root, presentation: filePresentation(body, name) });
    } catch (error) {
      if (token !== attempt.current) return;
      setView({ status: "error", message: FILE_VIEWER_COPY.error(displayMessageFor(error)) });
    }
  }, [connectionState, name, paneId, path]);

  useEffect(() => {
    void load();
  }, [load]);

  const reload = useCallback(() => {
    setView({ status: "loading" });
    void load();
  }, [load]);

  return { view, reload };
}
