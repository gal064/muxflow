import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import type { ActiveRoot, FileWorkspaceScope } from "../files/types";
import type { GitCommandResult, GitDiff, GitStatusSnapshot } from "./types";
import { gitScopeKey, type GitRepositoryHandle, type GitRepositoryStore } from "./repositoryStore";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";

interface Params {
  repositories: GitRepositoryStore;
  scope?: FileWorkspaceScope;
  root?: ActiveRoot;
  repositoryId?: string;
  path?: string;
  originalPath?: string;
  target?: GitDiff["target"];
}

/**
 * The paint ticket a rendered diff has to finish.
 *
 * The measurement starts when the request does and ends when the pixels land,
 * so it necessarily spans the two halves of this feature. Rather than pretend
 * otherwise, the request half hands the render half exactly the three values it
 * needs: the ticket, the load it belongs to, and the load React has committed.
 */
export interface DiffPaint {
  pending: MutableRefObject<PaintTicket | undefined>;
  lifecycle: MutableRefObject<number>;
  committed: MutableRefObject<number>;
}

export interface SharedGitDiff {
  diff?: GitDiff;
  status?: GitStatusSnapshot;
  loading: boolean;
  /** This surface's own failure, or the shared observation's if it has none. */
  error?: string;
  repository?: GitRepositoryHandle;
  /** A person asking again: re-reads the repository, then decides. */
  refresh(): Promise<void>;
  /** Runs one Git command and the single reload it owes. */
  command(run: () => Promise<GitCommandResult>): Promise<GitCommandResult | undefined>;
  fail(message: string): void;
  paint: DiffPaint;
}

/**
 * One diff tab's view of a shared repository observation.
 *
 * Everything about *when* a diff is read lives here: the lease on the shared
 * repository, the subscription that decides a re-read is owed, the single
 * in-flight request and its cancellation, and the reload a mutation owes. The
 * surface that renders the result owns none of it.
 */
export function useSharedGitDiff(params: Params): SharedGitDiff {
  const [diff, setDiff] = useState<GitDiff>();
  const [status, setStatus] = useState<GitStatusSnapshot>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  // The shared observation's error, held separately so that clearing it does
  // not also clear an error this surface raised itself.
  const [sharedError, setSharedError] = useState<string>();
  const [repository, setRepository] = useState<GitRepositoryHandle>();
  const serial = useRef(0);
  const committedLoadSerial = useRef(0);
  const requestedGeneration = useRef<string | undefined>(undefined);
  const abort = useRef<AbortController | undefined>(undefined);
  // A command owns its own reload. While one is running the shared observation
  // will publish the command's authoritative status, and reacting to that would
  // start the same reload a second time.
  const commanding = useRef(false);
  const pendingDiffPaint = useRef<PaintTicket | undefined>(undefined);
  const handle = useRef<GitRepositoryHandle | undefined>(undefined);

  const { repositories, repositoryId, path, originalPath, target } = params;
  // Memoized on the identity that keys the acquisition effect, so the effect
  // and the scope it acquires cannot describe different repositories.
  const boundScope = useMemo(
    () => (params.scope && params.root ? { scope: params.scope, root: params.root } : undefined),
    [params.scope, params.root],
  );
  const scopeIdentity = boundScope ? gitScopeKey(boundScope.scope, boundScope.root) : "";

  /**
   * One round trip. The response states the authoritative status it was read
   * against, so this surface never asks for status first, and a second tab on
   * the same repository reuses the shared observation instead of starting its
   * own discovery and status pipeline.
   */
  const load = useCallback(async (clearStale = false) => {
    const owner = handle.current;
    if (!owner || !repositoryId || !path || !target) return;
    const current = ++serial.current;
    // Claimed before awaiting so a watch event describing the same status
    // cannot start a second read of the same thing.
    requestedGeneration.current = owner.state().status?.generation;
    // A superseded load stops its own request, including the bulk body stream
    // it may already have started. Peers waiting on the same diff do not.
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    setLoading(true);
    pendingDiffPaint.current?.abandon();
    pendingDiffPaint.current = undefined;
    const paint = createPaintTicket(["workflow.git.diffPaint"], current);
    if (clearStale) {
      setDiff(undefined);
      setStatus(undefined);
      setError(undefined);
    }
    try {
      // The shared observation already knows whether this file still has the
      // change this tab is showing. When it does not, there is no diff to ask
      // for at all, which is what keeps a mutation to one request.
      const shared = owner.state().status;
      if (shared && shared.repository.id !== repositoryId) {
        // A saved tab whose root now resolves to a different repository has no
        // diff to ask for. Failing here keeps that explicit instead of sending
        // a request that can only be refused.
        throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      }
      if (shared && !entryStillChanged(shared, path, target)) {
        paint.abandon();
        setStatus(shared);
        setDiff(undefined);
        setError(undefined);
        return;
      }
      const result = await owner.diff({
        repositoryId,
        path,
        ...(originalPath ? { originalPath } : {}),
        target,
      }, controller.signal);
      if (current !== serial.current) {
        paint.abandon();
        return;
      }
      if (result.status.repository.id !== repositoryId) {
        throw new Error("This diff belongs to a different repository. Return to its workspace or close the tab.");
      }
      // The response states which status it was read against, which may be
      // newer than the one this load set out from. Claiming it stops the
      // subscriber from discarding a perfectly authoritative diff as stale.
      requestedGeneration.current = result.status.generation;
      setStatus(result.status);
      if (!entryStillChanged(result.status, path, target)) {
        paint.abandon();
        setDiff(undefined);
        setError(undefined);
        return;
      }
      setDiff(result.diff);
      pendingDiffPaint.current = paint;
      setError(undefined);
    } catch (cause) {
      paint.abandon();
      // The attempted generation is deliberately retained: a failure that the
      // repository state has not moved past must not be retried on every
      // subsequent watch publication. Retry is the user's, through the button.
      if (current !== serial.current || isAbort(cause)) return;
      setError(String(cause));
    } finally {
      if (current === serial.current) setLoading(false);
    }
  }, [originalPath, path, repositoryId, target]);
  // The subscription outlives any one `load`: its lifetime is the shared
  // observation's, and the diff this tab wants can change without the
  // repository changing. Reading the current `load` through a ref is what keeps
  // those two lifetimes independent without ever invoking a stale one.
  const currentLoad = useRef(load);
  currentLoad.current = load;

  // The shared repository observation. Acquiring it is what makes a matching
  // diff tab free: it joins the sidebar's watch rather than opening its own.
  useEffect(() => {
    if (!scopeIdentity || !boundScope || !repositoryId) return;
    const lease = repositories.acquire(boundScope.scope, boundScope.root);
    const acquired = lease.handle;
    handle.current = acquired;
    setRepository(acquired);
    // Loading is driven by the shared status, never ahead of it: reading a diff
    // before the repository is observed would fetch against an unknown state
    // and then immediately fetch again.
    const loadWhenStatusMoves = () => {
      const next = acquired.state();
      setSharedError(next.error);
      // The observation has answered, even if the answer is that it failed.
      // Nothing else clears this: `load` is the only other place that does, and
      // it never runs without a status.
      if (!next.loading) setLoading(false);
      if (commanding.current || !next.status) return;
      if (next.status.generation === requestedGeneration.current) return;
      void currentLoad.current();
    };
    loadWhenStatusMoves();
    const stop = acquired.subscribe(loadWhenStatusMoves);
    return () => {
      serial.current += 1;
      committedLoadSerial.current = 0;
      pendingDiffPaint.current?.abandon();
      pendingDiffPaint.current = undefined;
      requestedGeneration.current = undefined;
      abort.current?.abort();
      abort.current = undefined;
      stop();
      lease.release();
      handle.current = undefined;
      setRepository(undefined);
    };
    // `scopeIdentity` is the complete key of `boundScope`, and `load` is
    // reached only through `currentLoad`; neither belongs in this lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repositories, repositoryId, scopeIdentity]);

  /**
   * What the Refresh and Retry controls mean.
   *
   * `load` deliberately answers from the shared observation when that already
   * says this file has no such change. That is right for an automatic reload
   * and wrong for a person asking again, so an explicit refresh re-reads the
   * repository first and then decides.
   */
  const refresh = useCallback(async () => {
    const before = requestedGeneration.current;
    await handle.current?.refresh();
    // A refresh that moved the repository has already started the reload
    // through the subscription; forcing a second one here would be the same
    // request twice. Only a refresh that changed nothing still owes a read.
    if (handle.current?.state().status?.generation === before) await load(true);
  }, [load]);

  /**
   * One request and one post-command refresh. The result carries the
   * authoritative status, which the shared observation adopts; the only thing
   * still owed afterwards is this file's remaining diff, not a fresh
   * status-then-diff chain.
   */
  const command = useCallback(async (run: () => Promise<GitCommandResult>) => {
    commanding.current = true;
    try {
      const result = await run();
      // The shared observation already reconciled the command's authoritative
      // status, so the only thing still owed is this file's remaining diff.
      commanding.current = false;
      await load(true);
      return result;
    } catch (cause) {
      setError(String(cause));
      return undefined;
    } finally {
      commanding.current = false;
    }
  }, [load]);

  return {
    diff,
    status,
    loading,
    error: error ?? sharedError,
    repository,
    refresh,
    command,
    fail: setError,
    paint: { pending: pendingDiffPaint, lifecycle: serial, committed: committedLoadSerial },
  };
}

/** Whether the status still reports the change this tab is displaying. */
function entryStillChanged(status: GitStatusSnapshot, path: string, target: GitDiff["target"]): boolean {
  const entry = status.entries.find((candidate) => candidate.path === path);
  if (!entry) return false;
  return target === "staged" ? entry.indexKind !== "none" : entry.worktreeKind !== "none";
}

/** The one boundary that means "this was cancelled", not "this failed". */
function isAbort(cause: unknown): boolean {
  return cause instanceof DOMException && cause.name === "AbortError";
}
