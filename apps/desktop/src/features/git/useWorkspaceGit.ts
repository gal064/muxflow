import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import { gitScopeKey, type GitRepositoryState, type GitRepositoryStore } from "./repositoryStore";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

/**
 * What the panel renders, and the observation it acts through.
 *
 * This is the store's own published state: the handle travels inside it, so a
 * consumer can never pair one repository's status with another's observation.
 */
export type WorkspaceGitState = GitRepositoryState;

const IDLE: GitRepositoryState = { loading: false };

/**
 * The sidebar's view of the shared repository observation.
 *
 * This holds no watch, no status request and no cache of its own: it subscribes
 * to the one shared entry for its scope and renders whatever that entry has. A
 * panel opened beside a diff tab of the same repository therefore costs no
 * round trip at all, and paints that repository's current status on its first
 * render rather than after one.
 */
export function useWorkspaceGit(
  store: GitRepositoryStore,
  scope: FileWorkspaceScope | undefined,
  root: ActiveRoot | undefined,
): WorkspaceGitState {
  const observable = Boolean(scope && root?.gitWorktree);
  const bound = observable && scope && root ? { scope, root } : undefined;
  const identity = bound ? gitScopeKey(bound.scope, bound.root) : "";
  // Acquisition and subscription are one lifetime, so the entry is held for
  // exactly as long as this component is listening to it. Both closures capture
  // the scope of the render that produced `identity`, so a scope change can
  // never read one repository's state under another's key.
  const subscribe = useCallback((listener: () => void) => {
    if (!bound) return () => undefined;
    const lease = store.acquire(bound.scope, bound.root);
    const stop = lease.handle.subscribe(listener);
    listener();
    return () => {
      stop();
      lease.release();
    };
    // `identity` is the complete key of `bound` for every purpose here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identity, store]);
  const snapshot = useCallback(
    () => (bound ? store.peek(bound.scope, bound.root) : IDLE),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [identity, store],
  );
  const state = useSyncExternalStore(subscribe, snapshot, snapshot);

  // One panel paint span per observed repository, opened when the observation
  // starts and closed by the first status it produces.
  const panelPaint = useRef<PaintTicket | undefined>(undefined);
  const lifecycle = useRef(0);
  useEffect(() => {
    if (!identity) return;
    const generation = ++lifecycle.current;
    const ticket = createPaintTicket(["workflow.git.panelPaint"], generation);
    panelPaint.current = ticket;
    return () => {
      ticket.abandon();
      if (panelPaint.current === ticket) panelPaint.current = undefined;
    };
  }, [identity]);
  useEffect(() => {
    const ticket = panelPaint.current;
    if (!ticket || !state.status) return;
    panelPaint.current = undefined;
    ticket.afterPaint((candidate) => candidate.lifecycleGeneration === lifecycle.current);
  }, [state.status]);

  return state;
}
