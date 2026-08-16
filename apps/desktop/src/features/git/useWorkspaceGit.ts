import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitCommandResult, GitMutationRequest, GitStatusSnapshot } from "./types";
import type { GitRepositoryHandle, GitRepositoryState, GitRepositoryStore } from "./repositoryStore";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

export interface WorkspaceGitState {
  status?: GitStatusSnapshot;
  loading: boolean;
  error?: string;
  refresh(): Promise<void>;
  accept(status: GitStatusSnapshot): void;
  mutate(repositoryId: string, request: GitMutationRequest): Promise<GitCommandResult>;
  prepareDiscard(repositoryId: string, request: GitMutationRequest): Promise<string>;
  commit(repositoryId: string, expectedStatusGeneration: string, message: string): Promise<GitCommandResult>;
}

const IDLE: GitRepositoryState = { loading: false };

/** Runs against the live observation, or refuses because there is not one. */
function observed<T>(
  handle: GitRepositoryHandle | undefined,
  run: (owner: GitRepositoryHandle) => Promise<T>,
): Promise<T> {
  return handle ? run(handle) : Promise.reject(new Error("This repository is no longer observed."));
}

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
  const identity = bound
    ? `${bound.scope.clientId}\0${bound.scope.serverIdentity}\0${bound.scope.terminalEpoch}\0${bound.root.token}\0${bound.root.path}`
    : "";
  const handle = useRef<GitRepositoryHandle | undefined>(undefined);

  // Acquisition and subscription are one lifetime, so the entry is held for
  // exactly as long as this component is listening to it. Both closures capture
  // the scope of the render that produced `identity`, so a scope change can
  // never read one repository's state under another's key.
  const subscribe = useCallback((listener: () => void) => {
    if (!bound) return () => undefined;
    const acquired = store.acquire(bound.scope, bound.root);
    handle.current = acquired;
    const stop = acquired.subscribe(listener);
    listener();
    return () => {
      stop();
      acquired.release();
      handle.current = undefined;
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

  const refresh = useCallback(async () => {
    await handle.current?.refresh();
  }, []);
  const accept = useCallback((status: GitStatusSnapshot) => {
    handle.current?.accept(status);
  }, []);
  const mutate = useCallback(
    (repositoryId: string, request: GitMutationRequest) =>
      observed(handle.current, (owner) => owner.mutate(repositoryId, request)),
    [],
  );
  const prepareDiscard = useCallback(
    (repositoryId: string, request: GitMutationRequest) =>
      observed(handle.current, (owner) => owner.prepareDiscard(repositoryId, request)),
    [],
  );
  const commit = useCallback(
    (repositoryId: string, expectedStatusGeneration: string, message: string) =>
      observed(handle.current, (owner) => owner.commit(repositoryId, expectedStatusGeneration, message)),
    [],
  );

  return {
    status: state.status,
    loading: observable ? state.loading : false,
    error: state.error,
    refresh,
    accept,
    mutate,
    prepareDiscard,
    commit,
  };
}
