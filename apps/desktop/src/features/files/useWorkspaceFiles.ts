import { useCallback, useEffect, useRef, useState } from "react";
import { keyForScope, keyForTransferConnection, sameRoot } from "./api";
import { measurePerf, openPerfSpan, recordPerfCounter } from "../../perf/probe";
import { isTerminalTransferState, mergeCanonicalTransfer } from "../transfers/transferState";
import type {
  ActiveRoot,
  DirectoryListing,
  FileWorkspaceClient,
  FileWorkspaceScope,
  TransferStatus,
  WorkspaceEvent,
} from "./types";

interface WorkspaceFilesState {
  scopeKey: string;
  transferConnectionKey: string;
  root?: ActiveRoot;
  listings: ReadonlyMap<string, DirectoryListing>;
  expanded: ReadonlySet<string>;
  loading: ReadonlySet<string>;
  /** Reads a person asked for and has not yet been answered. See `refresh`. */
  requestedReads: number;
  transfers: readonly TransferStatus[];
  error?: string;
}

const EMPTY = new Map<string, DirectoryListing>();

/**
 * How often the workspace root is re-resolved when nothing has changed. Every
 * event that *can* be pushed already re-resolves it immediately; this only
 * covers `cd` inside the current pane, which tmux does not announce.
 */
export const ACTIVE_ROOT_POLL_MS = 2_000;

/**
 * How long one directory's filesystem events are gathered before it is re-read.
 *
 * Every event used to re-read immediately, so a directory an agent is writing
 * into was re-listed once per write — over SSH that is a round trip per write,
 * all of them asking the same question. The window is a throttle rather than a
 * debounce (it starts at the first event of a burst and is not pushed back by
 * later ones), because a directory under continuous change must still refresh
 * on a bounded schedule rather than only once the writing stops.
 */
export const DIRECTORY_REFRESH_COALESCE_MS = 150;

export function useWorkspaceFiles(client: FileWorkspaceClient, scope: FileWorkspaceScope | undefined) {
  const [state, setState] = useState<WorkspaceFilesState>({
    scopeKey: "",
    transferConnectionKey: "",
    listings: EMPTY,
    expanded: new Set(),
    loading: new Set(),
    requestedReads: 0,
    transfers: [],
  });
  const stateRef = useRef(state);
  stateRef.current = state;
  const scopeEpoch = useRef(0);
  const rootProbeSerial = useRef(0);
  const directorySerial = useRef(new Map<string, number>());
  const refreshTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const scopeKey = scope ? keyForScope(scope) : "";
  const transferConnectionKey = scope ? keyForTransferConnection(scope) : "";
  const scopeRef = useRef(scope);
  scopeRef.current = scope;

  /**
   * Reads one directory, against the root the caller was authorised for.
   *
   * `root` is a parameter rather than something read from `stateRef` here,
   * because a caller can be *deferred* — the coalescing window below holds one
   * for 150 ms — and the root can move underneath it. Reading it ambiently made
   * "the root that authorised this read" and "the root at the moment it went
   * out" the same variable, so a delayed caller silently listed an old root's
   * path against the new one, and none of the completion guards could see it:
   * they compare against the root the request carried, which was the new one.
   * One guard here covers every caller, including the next deferred one.
   */
  const loadDirectory = useCallback(async (root: ActiveRoot, path: string, force = false, append = false) => {
    const activeScope = scopeRef.current;
    if (!activeScope || keyForScope(activeScope) !== scopeKey) return;
    if (!sameRoot(stateRef.current.root, root)) return;
    const previous = stateRef.current.listings.get(path);
    if (!force && !append && previous) {
      recordPerfCounter("explorer.cacheHits");
      return;
    }
    recordPerfCounter("explorer.cacheMisses");
    if (append && (!previous?.nextPageToken || previous.complete)) return;
    const epoch = scopeEpoch.current;
    const serial = (directorySerial.current.get(path) ?? 0) + 1;
    directorySerial.current.set(path, serial);
    setState((current) => ({ ...current, loading: new Set(current.loading).add(path), error: undefined }));
    try {
      // What a directory read costs on this link, when `ADE_PERF_LOG` is set and
      // nothing otherwise. The flicker this hook was reported for is only ever
      // visible when this number is large, and until now nothing measured it.
      const listing = await measurePerf(
        append ? "files.listDirectory.page" : "files.listDirectory",
        () => client.listDirectory(activeScope, root, path, append ? previous?.nextPageToken : undefined),
      );
      if (epoch !== scopeEpoch.current || directorySerial.current.get(path) !== serial || keyForScope(activeScope) !== scopeKey) return;
      setState((current) => {
        if (!sameRoot(current.root, root) || listing.rootToken !== root.token) return current;
        const listings = new Map(current.listings);
        listings.set(path, append && previous ? {
          ...listing,
          entries: mergeEntries(previous.entries, listing.entries),
          overflowRecovery: previous.overflowRecovery || listing.overflowRecovery,
        } : listing);
        const loading = new Set(current.loading);
        loading.delete(path);
        return { ...current, listings, loading };
      });
    } catch (error) {
      setState((current) => {
        const loading = new Set(current.loading);
        loading.delete(path);
        return sameRoot(current.root, root) ? { ...current, loading, error: String(error) } : current;
      });
    }
  }, [client, scopeKey]);

  /**
   * Re-reads a directory that filesystem events say has changed, at most once
   * per `DIRECTORY_REFRESH_COALESCE_MS`.
   *
   * The timer is started by the first event of a burst and deliberately not
   * pushed back by the ones behind it: a directory an agent is writing into
   * continuously would otherwise never be re-read at all.
   *
   * The root token the event arrived under is carried through the wait and
   * re-checked on the far side. Before there was a wait, the caller's "is this
   * event for the root we are showing?" test and the request it authorised were
   * the same instant; a delay puts a root change between them — the active root
   * is re-resolved every two seconds, and an agent's `cd` moves it — and without
   * this the timer would list a path from the old root against the new one. The
   * completion guards cannot catch that, because they compare against the root
   * the request was issued with, which is the new one.
   */
  const coalesceRefresh = useCallback((root: ActiveRoot, path: string) => {
    const timers = refreshTimers.current;
    const key = `${root.token}\0${path}`;
    if (timers.has(key)) return;
    timers.set(key, setTimeout(() => {
      timers.delete(key);
      void loadDirectory(root, path, true);
    }, DIRECTORY_REFRESH_COALESCE_MS));
  }, [loadDirectory]);

  const applyEvent = useCallback((event: WorkspaceEvent) => {
    const current = stateRef.current;
    if (event.kind === "rootChanged") {
      // Root responses are also returned directly to the poller, where the
      // scope epoch and operation serial reject completions from the previous
      // pane. The broadcast has no caller epoch, so admitting it here would
      // reintroduce the stale cross-pane race that those barriers prevent.
      return;
    }
    if (event.kind === "transfer") {
      if (event.transfer.scopeKey !== transferConnectionKey || !scopeRef.current
        || keyForTransferConnection(scopeRef.current) !== transferConnectionKey) return;
      setState((value) => ({ ...value, transfers: upsertTransfer(value.transfers, event.transfer) }));
      return;
    }
    if (!current.root || event.rootToken !== current.root.token) return;
    if (event.kind === "directoryChanged") {
      openPerfSpan("explorer.externalChangeToPaint");
      coalesceRefresh(current.root, event.directory);
    } else if (event.kind === "fileChanged" || event.kind === "fileDeleted") {
      openPerfSpan("explorer.externalChangeToPaint");
      coalesceRefresh(current.root, parentPath(event.path));
    }
  }, [coalesceRefresh]);

  useEffect(() => {
    scopeEpoch.current += 1;
    if (!scope) {
      setState((current) => ({
        scopeKey: "", transferConnectionKey: "", listings: new Map(), expanded: new Set(), loading: new Set(),
        requestedReads: 0,
        transfers: current.transfers.map(staleTransferOnScopeReplacement),
      }));
      return;
    }
    setState((current) => ({
      scopeKey,
      transferConnectionKey,
      listings: new Map(),
      expanded: new Set(),
      loading: new Set(),
      requestedReads: 0,
      transfers: !current.transferConnectionKey || current.transferConnectionKey === transferConnectionKey
        ? current.transfers
        : current.transfers.map(staleTransferOnScopeReplacement),
    }));
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    const epoch = scopeEpoch.current;
    void client.subscribe(scope, applyEvent).then((stop) => {
      if (disposed) stop();
      else unsubscribe = stop;
    }).catch((error) => { if (!disposed) setState((value) => ({ ...value, error: String(error) })); });
    let resolving = false;
    const resolve = async () => {
      if (resolving) return;
      resolving = true;
      const probe = ++rootProbeSerial.current;
      openPerfSpan("workflow.explorer.rootPaint");
      try {
        const activeScope = scopeRef.current;
        if (!activeScope || keyForScope(activeScope) !== scopeKey) return;
        const root = await client.resolveActiveRoot(activeScope);
        if (disposed || epoch !== scopeEpoch.current || probe !== rootProbeSerial.current) return;
        setState((current) => sameRoot(current.root, root) ? current : {
          ...current,
          scopeKey,
          root,
          listings: new Map(),
          expanded: new Set([root.path]),
          loading: new Set(),
          error: undefined,
        });
      } catch (error) {
        if (!disposed && epoch === scopeEpoch.current && probe === rootProbeSerial.current) setState((current) => ({ ...current, error: String(error) }));
      } finally {
        resolving = false;
      }
    };
    void resolve();
    // A backstop, not the primary path. Pane and window changes already rebuild
    // this scope and re-resolve immediately, so the only thing left for a timer
    // to catch is the user running `cd` inside the pane they are already in —
    // for which tmux emits no notification at all, so nothing can push it.
    // At 350 ms this was a host round trip three times a second forever, which
    // over SSH is three round trips a second on an idle connection.
    const poll = window.setInterval(() => { void resolve(); }, ACTIVE_ROOT_POLL_MS);
    return () => {
      disposed = true;
      scopeEpoch.current += 1;
      window.clearInterval(poll);
      // A refresh still waiting out its window belongs to the scope that is
      // going away; letting it fire would read a directory for a pane the user
      // has already left.
      for (const timer of refreshTimers.current.values()) clearTimeout(timer);
      refreshTimers.current.clear();
      unsubscribe?.();
    };
  }, [applyEvent, client, scopeKey]);

  useEffect(() => {
    if (state.root && !state.listings.has(state.root.path)) void loadDirectory(state.root, state.root.path);
  }, [loadDirectory, state.listings, state.root]);

  useEffect(() => {
    const activeScope = scopeRef.current;
    if (!activeScope || !state.root) return;
    let disposed = false;
    let releases: (() => void)[] = [];
    void Promise.allSettled([...state.expanded].map((directory) => client.acquireDirectoryWatch(activeScope, state.root!, directory))).then((results) => {
      const next = results.filter((result): result is PromiseFulfilledResult<Awaited<ReturnType<FileWorkspaceClient["acquireDirectoryWatch"]>>> => result.status === "fulfilled").map((result) => result.value);
      if (disposed) next.forEach((lease) => lease.release());
      else {
        releases = next.map((lease) => lease.release);
        setState((current) => {
          if (!sameRoot(current.root, state.root)) return current;
          const listings = new Map(current.listings);
          for (const lease of next) if (lease.snapshot.rootToken === state.root!.token) listings.set(lease.snapshot.directory, lease.snapshot);
          return { ...current, listings };
        });
        const failure = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
        if (failure) setState((current) => sameRoot(current.root, state.root) ? { ...current, error: String(failure.reason) } : current);
      }
    });
    return () => { disposed = true; releases.forEach((release) => release()); };
  }, [client, scopeKey, state.expanded, state.root]);

  const toggleDirectory = useCallback((path: string) => {
    if (!stateRef.current.expanded.has(path)) {
      openPerfSpan("explorer.expandToPaint");
      openPerfSpan("workflow.explorer.directoryExpandPaint");
    }
    setState((current) => {
      const expanded = new Set(current.expanded);
      if (expanded.has(path)) expanded.delete(path);
      else expanded.add(path);
      return { ...current, expanded };
    });
    const root = stateRef.current.root;
    if (root && !stateRef.current.listings.has(path)) void loadDirectory(root, path);
  }, [loadDirectory]);

  /**
   * A read a person asked for, which is the one kind that owes them an answer.
   *
   * Tracked apart from `loading` because the view has to tell the two cases
   * apart: a refresh nobody asked for must show nothing that moves — that was
   * the flicker — while pressing Refresh and seeing nothing at all is a button
   * that looks broken on exactly the slow link that makes a refresh worth
   * pressing. A count, not a flag, so two overlapping presses do not have the
   * first one's completion clear the second one's signal.
   */
  const refresh = useCallback((directory?: string) => {
    const root = stateRef.current.root;
    const target = directory ?? root?.path;
    if (!root || !target) return;
    setState((current) => ({ ...current, requestedReads: current.requestedReads + 1 }));
    void loadDirectory(root, target, true).finally(() => {
      setState((current) => ({ ...current, requestedReads: Math.max(0, current.requestedReads - 1) }));
    });
  }, [loadDirectory]);

  const recordTransfer = useCallback((transfer: TransferStatus) => {
    if (transfer.scopeKey !== transferConnectionKey) return;
    setState((current) => ({ ...current, transfers: upsertTransfer(current.transfers, transfer) }));
  }, [transferConnectionKey]);

  const loadMore = useCallback((directory: string) => {
    const root = stateRef.current.root;
    if (root) void loadDirectory(root, directory, false, true);
  }, [loadDirectory]);

  // Effects run after paint. Mask the prior pane synchronously on the render
  // where scopeKey changes so Explorer never flashes or acts on the old root.
  const visible = state.scopeKey === scopeKey ? state : {
    scopeKey,
    transferConnectionKey,
    listings: EMPTY,
    expanded: new Set<string>(),
    loading: new Set<string>(),
    requestedReads: 0,
    transfers: state.transferConnectionKey === transferConnectionKey
      ? state.transfers
      : state.transfers.map(staleTransferOnScopeReplacement),
  };
  return { ...visible, toggleDirectory, refresh, recordTransfer, loadMore };
}

function staleTransferOnScopeReplacement(transfer: TransferStatus): TransferStatus {
  if (isTerminalTransferState(transfer.state)) return transfer;
  return {
    ...transfer,
    state: "failed",
    outcome: transfer.state === "verifying" ? "unknown" : "notPublished",
    failureKind: "staleScope",
    error: "Transfer scope was replaced before its authoritative terminal event arrived.",
  };
}

function upsertTransfer(transfers: readonly TransferStatus[], next: TransferStatus): TransferStatus[] {
  const index = transfers.findIndex((item) => item.id === next.id);
  if (index < 0) return [next, ...transfers].slice(0, 100);
  const copy = [...transfers];
  copy[index] = mergeCanonicalTransfer(copy[index], next);
  return copy;
}

function mergeEntries(previous: readonly DirectoryListing["entries"][number][], next: readonly DirectoryListing["entries"][number][]) {
  const byPath = new Map(previous.map((entry) => [entry.path, entry]));
  for (const entry of next) byPath.set(entry.path, entry);
  return [...byPath.values()];
}

function parentPath(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}
