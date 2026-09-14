// Files state that outlives one screen (design doc §9.6, §11.2).
//
// The root capability is per pane and is shared by the listing screens and the
// viewer, so it lives here rather than in a screen's state: pushing
// `/files/<pane>/dir` or `/file/<pane>` must not cost another
// RESOLVE_ACTIVE_ROOT round trip.

import { createStore, type StoreApi } from "zustand/vanilla";
import { EventKind, type HostEvent } from "../../protocol/gen/envelope_pb";
import type { ResolvedRoot } from "./activeRoot";

export interface FilesState {
  /** paneId → the root capability last resolved for it. */
  roots: Record<string, ResolvedRoot>;
  /** Exact terminal-resolved file path → its possibly narrow, non-listable read capability. */
  terminalFileRoots: Record<string, ResolvedRoot>;
  /**
   * Directory path → a counter bumped whenever the host says that directory
   * changed. A mounted listing watches its own entry and re-lists silently
   * (§9.6). Keys are host path strings, used only for identity.
   */
  directoryRevisions: Record<string, number>;
}

export interface FilesActions {
  setRoot(root: ResolvedRoot): void;
  setTerminalFileRoot(path: string, root: ResolvedRoot): void;
  clearRoot(paneId: string): void;
  bumpDirectory(path: string): void;
  /** ACTIVE_ROOT / DIRECTORY_SNAPSHOT / FILE_CHANGED, forwarded by the connection (§7.4). */
  applyFileEvent(event: HostEvent): void;
  /** Every root token is bound to one connection; a reconnect invalidates them all. */
  clearAll(): void;
}

export type FilesStore = StoreApi<FilesState & FilesActions>;

export function createFilesStore(): FilesStore {
  return createStore<FilesState & FilesActions>((set, get) => ({
    roots: {},
    terminalFileRoots: {},
    directoryRevisions: {},

    setRoot(root) {
      set({ roots: { ...get().roots, [root.paneId]: root } });
    },

    setTerminalFileRoot(path, root) {
      set({ terminalFileRoots: { ...get().terminalFileRoots, [terminalFileKey(root.paneId, path)]: root } });
    },

    clearRoot(paneId) {
      const roots = { ...get().roots };
      delete roots[paneId];
      set({ roots });
    },

    bumpDirectory(path) {
      if (!path) return;
      const revisions = get().directoryRevisions;
      set({ directoryRevisions: { ...revisions, [path]: (revisions[path] ?? 0) + 1 } });
    },

    applyFileEvent(event) {
      switch (event.kind) {
        case EventKind.ACTIVE_ROOT: {
          const activeRoot = event.file?.activeRoot;
          if (!activeRoot?.paneId || !activeRoot.root || !activeRoot.rootToken) return;
          get().setRoot({
            paneId: activeRoot.paneId,
            root: activeRoot.root,
            rootToken: activeRoot.rootToken,
            gitWorktree: activeRoot.gitWorktree,
            rootGeneration: activeRoot.rootGeneration,
          });
          return;
        }
        case EventKind.DIRECTORY_SNAPSHOT:
          get().bumpDirectory(event.file?.directory?.path ?? "");
          return;
        case EventKind.FILE_CHANGED: {
          // The event names the file; the listing that has to re-read is its
          // parent. The derived string is a cache key only — it is never sent
          // back to the host as an operation target.
          const path = event.file?.metadata?.path ?? "";
          const slash = path.lastIndexOf("/");
          if (slash > 0) get().bumpDirectory(path.slice(0, slash));
          return;
        }
        default:
          return;
      }
    },

    clearAll() {
      set({ roots: {}, terminalFileRoots: {}, directoryRevisions: {} });
    },
  }));
}

/** The app-wide instance. Tests create their own with `createFilesStore()`. */
export const filesStore: FilesStore = createFilesStore();

export function terminalFileKey(paneId: string, path: string): string {
  return `${paneId}\0${path}`;
}
