import { useCallback, useEffect, useRef, useState } from "react";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitStatusSnapshot } from "./types";
import type { GitRepositoryHandle, GitRepositoryState, GitRepositoryStore } from "./repositoryStore";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

export interface WorkspaceGitState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
  refresh(): Promise<void>;
  accept(status: GitStatusSnapshot): void;
}

/**
 * The sidebar's view of the shared repository observation.
 *
 * This holds no watch, no status request and no cache of its own: it acquires
 * the one shared entry for its scope and renders whatever that entry has. A
 * panel opened beside a diff tab of the same repository therefore costs no
 * round trip at all, and paints that repository's current status on its first
 * render rather than after one.
 */
export function useWorkspaceGit(
  store: GitRepositoryStore,
  scope: FileWorkspaceScope | undefined,
  root: ActiveRoot | undefined,
): WorkspaceGitState {
  const [, setRevision] = useState(0);
  const handle = useRef<GitRepositoryHandle | undefined>(undefined);
  const pendingPanelPaint = useRef<PaintTicket | undefined>(undefined);
  const lifecycle = useRef(0);
  const observable = Boolean(scope && root?.gitWorktree);
  const identity = observable && root && scope
    ? `${scope.clientId}\0${scope.serverIdentity}\0${scope.terminalEpoch}\0${root.token}\0${root.path}`
    : "";
  // The effect below owns acquisition, but rendering must not wait for it: a
  // warm repository is already observed and its status is available now.
  const target = useRef<{ scope: FileWorkspaceScope; root: ActiveRoot } | undefined>(undefined);
  target.current = scope && root ? { scope, root } : undefined;

  useEffect(() => {
    if (!identity) {
      handle.current = undefined;
      return;
    }
    const bound = target.current;
    if (!bound) return;
    const generation = ++lifecycle.current;
    const acquired = store.acquire(bound.scope, bound.root);
    handle.current = acquired;
    const paint = createPaintTicket(["workflow.git.panelPaint"], generation);
    pendingPanelPaint.current = paint;
    const settle = () => {
      if (pendingPanelPaint.current !== paint || !acquired.state().status) return;
      pendingPanelPaint.current = undefined;
      paint.afterPaint((ticket) => ticket.lifecycleGeneration === lifecycle.current);
    };
    const stop = acquired.subscribe(() => {
      setRevision((value) => value + 1);
      settle();
    });
    setRevision((value) => value + 1);
    settle();
    return () => {
      lifecycle.current += 1;
      paint.abandon();
      if (pendingPanelPaint.current === paint) pendingPanelPaint.current = undefined;
      stop();
      acquired.release();
      handle.current = undefined;
    };
  }, [identity, store]);

  const refresh = useCallback(async () => {
    await handle.current?.refresh();
  }, []);
  const accept = useCallback((status: GitStatusSnapshot) => {
    handle.current?.accept(status);
  }, []);

  const state: GitRepositoryState | undefined = identity && target.current
    ? (handle.current?.state() ?? store.peek(target.current.scope, target.current.root))
    : undefined;
  return {
    status: state?.status,
    loading: observable ? (state?.loading ?? true) : false,
    error: state?.error,
    refresh,
    accept,
  };
}
