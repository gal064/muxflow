import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { keyForScope, keyForTransferConnection, sameRoot } from "./api";
import { measurePerfOutcome, recordPerfCounter } from "../../perf/probe";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";
import { isTerminalTransferState, mergeCanonicalTransfer } from "../transfers/transferState";
import { DirectoryListingCache } from "./directoryCache";
import {
  appendPage,
  isRecoveryReason,
  parentPath,
  patchEntry,
  reachableWatchTargets,
  removeEntry,
  type RecoveryReason,
} from "./listingModel";
import { DirectoryWatchLeases } from "./watchLeases";
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
const EXTERNAL_CHANGE_PAINT = ["explorer.externalChangeToPaint"] as const;
type DirectoryLoadResult = "applied" | "stale" | "failed";

/**
 * How often the active root is re-checked when nothing has announced a change,
 * and only while this window is in the foreground.
 *
 * Every event that *can* be pushed already re-resolves it immediately; this
 * covers `cd` inside the current pane, which tmux does not announce. It is a
 * backstop rather than a pipeline: a hidden window checks nothing at all, and
 * the host answers an unchanged root from the caller's own capability without a
 * second authoritative discovery or a broadcast payload.
 */
export const ACTIVE_ROOT_BACKSTOP_MS = 15_000;

/**
 * How long one directory's *recovery* is deferred before it is re-read.
 *
 * Only events a cached listing could not answer reach this. Precise events with
 * a mapped entry patch the listing in place and cost no request at all, so this
 * window now throttles genuine gaps — overflow, an incomplete page, an entry
 * the host could not map — rather than ordinary file writes.
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
  const listAborts = useRef(new Map<string, AbortController>());
  const refreshTimers = useRef(new Map<string, { timer: ReturnType<typeof setTimeout>; paint: PaintTicket }>());
  const paintGenerations = useRef(new Map<string, number>());
  /** Expansions whose paint is owed by a watch bootstrap that has not landed. */
  const pendingExpandPaints = useRef(new Map<string, { paint: PaintTicket; generation: number }>());
  const cache = useRef(new DirectoryListingCache());
  const leases = useRef(new DirectoryWatchLeases());
  const scopeKey = scope ? keyForScope(scope) : "";
  const transferConnectionKey = scope ? keyForTransferConnection(scope) : "";
  const scopeRef = useRef(scope);
  scopeRef.current = scope;

  /** Stops bounded remote work for reads nothing will read any more. */
  const abortListing = useCallback((predicate: (path: string) => boolean) => {
    for (const [path, controller] of [...listAborts.current]) {
      if (!predicate(path)) continue;
      listAborts.current.delete(path);
      controller.abort();
    }
  }, []);

  /**
   * Installs an authoritative listing for one directory.
   *
   * The one place a listing enters the tree, so caching, loading state, and the
   * root-token guard cannot drift apart between the watch bootstrap, an
   * authoritative rescan, and a recovery list.
   */
  const applyListing = useCallback((root: ActiveRoot, directory: string, listing: DirectoryListing) => {
    const activeScope = scopeRef.current;
    if (!activeScope || listing.rootToken !== root.token) return;
    cache.current.set({ clientId: activeScope.clientId, rootToken: root.token, directory }, listing);
    setState((current) => {
      if (!sameRoot(current.root, root) || listing.rootToken !== root.token) return current;
      // Only directories the tree is actually showing. A snapshot that races a
      // collapse, or a recovery list whose directory was deleted underneath it,
      // must not put rows back into a tree that no longer reaches them.
      if (!current.expanded.has(directory)) return current;
      const listings = new Map(current.listings);
      listings.set(directory, listing);
      const loading = new Set(current.loading);
      loading.delete(directory);
      return { ...current, listings, loading };
    });
  }, []);

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
  const loadDirectory = useCallback(async (
    root: ActiveRoot,
    path: string,
    append = false,
  ): Promise<DirectoryLoadResult> => {
    const activeScope = scopeRef.current;
    if (!activeScope || keyForScope(activeScope) !== scopeKey) return "stale";
    if (!sameRoot(stateRef.current.root, root)) return "stale";
    const previous = stateRef.current.listings.get(path);
    if (append && (!previous?.nextPageToken || previous.complete)) return "stale";
    const epoch = scopeEpoch.current;
    const serial = (directorySerial.current.get(path) ?? 0) + 1;
    directorySerial.current.set(path, serial);
    const controller = new AbortController();
    listAborts.current.get(path)?.abort();
    listAborts.current.set(path, controller);
    setState((current) => ({ ...current, loading: new Set(current.loading).add(path), error: undefined }));
    try {
      // What a directory read costs on this link, when `ADE_PERF_LOG` is set and
      // nothing otherwise. The flicker this hook was reported for is only ever
      // visible when this number is large, and until now nothing measured it.
      const listing = await measurePerfOutcome(
        append ? "files.listDirectory.page" : "files.listDirectory",
        () => client.listDirectory(activeScope, root, path, {
          ...(append && previous?.nextPageToken ? { pageToken: previous.nextPageToken } : {}),
          signal: controller.signal,
        }),
      );
      if (listAborts.current.get(path) === controller) listAborts.current.delete(path);
      if (epoch !== scopeEpoch.current || directorySerial.current.get(path) !== serial || keyForScope(activeScope) !== scopeKey) return "stale";
      if (!sameRoot(stateRef.current.root, root) || listing.rootToken !== root.token) return "stale";
      applyListing(root, path, append && previous ? appendPage(previous, listing) : listing);
      return "applied";
    } catch (error) {
      if (listAborts.current.get(path) === controller) listAborts.current.delete(path);
      const aborted = controller.signal.aborted;
      setState((current) => {
        const loading = new Set(current.loading);
        loading.delete(path);
        if (!sameRoot(current.root, root)) return current;
        return aborted ? { ...current, loading } : { ...current, loading, error: String(error) };
      });
      return aborted ? "stale" : "failed";
    }
  }, [applyListing, client, scopeKey]);

  /**
   * Re-reads a directory whose cached listing could not answer an event, at
   * most once per `DIRECTORY_REFRESH_COALESCE_MS`.
   *
   * The timer is started by the first event of a burst and deliberately not
   * pushed back by the ones behind it: a directory an agent is writing into
   * continuously would otherwise never be re-read at all.
   *
   * The root token the event arrived under is carried through the wait and
   * re-checked on the far side. Before there was a wait, the caller's "is this
   * event for the root we are showing?" test and the request it authorised were
   * the same instant; a delay puts a root change between them, and without this
   * the timer would list a path from the old root against the new one. The
   * completion guards cannot catch that, because they compare against the root
   * the request was issued with, which is the new one.
   */
  const coalesceRecovery = useCallback((root: ActiveRoot, path: string, reason: RecoveryReason) => {
    const timers = refreshTimers.current;
    const key = `${root.token}\0${path}`;
    if (timers.has(key)) return;
    recordPerfCounter("explorer.recoveryLists");
    recordPerfCounter(`explorer.recovery.${reason}`);
    const paint = createPaintTicket(EXTERNAL_CHANGE_PAINT, scopeEpoch.current);
    const timer = setTimeout(() => {
      timers.delete(key);
      if (!sameRoot(stateRef.current.root, root)) {
        paint.abandon();
        return;
      }
      void loadDirectory(root, path).then((result) => {
        if (result !== "applied") {
          paint.abandon();
          return;
        }
        paint.afterPaint((ticket) => ticket.lifecycleGeneration === scopeEpoch.current
          && sameRoot(stateRef.current.root, root)
          && stateRef.current.expanded.has(path));
      });
    }, DIRECTORY_REFRESH_COALESCE_MS);
    timers.set(key, { timer, paint });
  }, [loadDirectory]);

  /**
   * Applies one precise change to the cached listing that owns it.
   *
   * A patch is measured, deliberate, and free: one remote file write moves one
   * row. Only an event a complete listing genuinely cannot represent falls
   * through to a recovery list.
   */
  const applyPrecise = useCallback((
    root: ActiveRoot,
    directory: string,
    patched: DirectoryListing | RecoveryReason,
  ) => {
    if (isRecoveryReason(patched)) {
      coalesceRecovery(root, directory, patched);
      return;
    }
    recordPerfCounter("explorer.listingPatches");
    const paint = createPaintTicket(EXTERNAL_CHANGE_PAINT, scopeEpoch.current);
    applyListing(root, directory, patched);
    paint.afterPaint((ticket) => ticket.lifecycleGeneration === scopeEpoch.current
      && sameRoot(stateRef.current.root, root)
      && stateRef.current.expanded.has(directory));
  }, [applyListing, coalesceRecovery]);

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
    const root = current.root;
    if (!root || event.rootToken !== root.token) return;
    if (event.kind === "directorySnapshot") {
      // Authoritative: the host re-listed and this *is* the directory now.
      recordPerfCounter("explorer.authoritativeSnapshots");
      applyListing(root, event.listing.directory, event.listing);
      return;
    }
    const directory = parentPath(event.path);
    if (event.kind === "fileChanged") {
      applyPrecise(root, directory, event.entry
        ? patchEntry(current.listings.get(directory), event.entry)
        : "unmappable");
      return;
    }
    // A deleted directory takes its whole cached subtree with it.
    setState((value) => pruneSubtree(value, event.path));
    abortListing((path) => path === event.path || path.startsWith(`${event.path}/`));
    applyPrecise(root, directory, removeEntry(current.listings.get(directory), event.path));
  }, [abortListing, applyListing, applyPrecise, transferConnectionKey]);

  useEffect(() => {
    scopeEpoch.current += 1;
    abortListing(() => true);
    if (!scope) {
      cache.current.clear();
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
      const rootPaint = createPaintTicket(["workflow.explorer.rootPaint"], epoch);
      try {
        const activeScope = scopeRef.current;
        if (!activeScope || keyForScope(activeScope) !== scopeKey) {
          rootPaint.abandon();
          return;
        }
        const known = stateRef.current.root?.token;
        const root = await client.resolveActiveRoot(activeScope, known ? { knownRootToken: known } : {});
        if (disposed || epoch !== scopeEpoch.current || probe !== rootProbeSerial.current) {
          rootPaint.abandon();
          return;
        }
        if (sameRoot(stateRef.current.root, root)) {
          rootPaint.abandon();
          return;
        }
        // A replaced root invalidates every path, listing, and content the
        // previous one authorised: the same path under a new capability is a
        // different file.
        abortListing(() => true);
        cache.current.invalidateOtherRoots(activeScope.clientId, root.token);
        setState((current) => ({
          ...current,
          scopeKey,
          root,
          listings: new Map(),
          expanded: new Set([root.path]),
          loading: new Set(),
          error: undefined,
        }));
        rootPaint.afterPaint((ticket) => !disposed
          && ticket.lifecycleGeneration === scopeEpoch.current
          && probe === rootProbeSerial.current
          && sameRoot(stateRef.current.root, root));
      } catch (error) {
        rootPaint.abandon();
        if (!disposed && epoch === scopeEpoch.current && probe === rootProbeSerial.current) setState((current) => ({ ...current, error: String(error) }));
      } finally {
        resolving = false;
      }
    };
    void resolve();
    // A foreground backstop, not a pipeline. Pane and window changes rebuild
    // this scope and re-resolve immediately, so the only thing left for a timer
    // to catch is `cd` inside the pane the user is already in — for which tmux
    // emits no notification at all. A hidden window checks nothing, and a
    // window that becomes visible checks once on the transition rather than
    // waiting out the interval.
    const foreground = () => typeof document === "undefined" || document.visibilityState === "visible";
    const backstop = window.setInterval(() => { if (foreground()) void resolve(); }, ACTIVE_ROOT_BACKSTOP_MS);
    const onVisibility = () => { if (foreground()) void resolve(); };
    document?.addEventListener?.("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      scopeEpoch.current += 1;
      window.clearInterval(backstop);
      document?.removeEventListener?.("visibilitychange", onVisibility);
      abortListing(() => true);
      // A refresh still waiting out its window belongs to the scope that is
      // going away; letting it fire would read a directory for a pane the user
      // has already left.
      for (const { timer, paint } of refreshTimers.current.values()) {
        clearTimeout(timer);
        paint.abandon();
      }
      refreshTimers.current.clear();
      paintGenerations.current.clear();
      unsubscribe?.();
    };
    // `scope` is deliberately not a dependency: `scopeKey` is its exact
    // identity, and an unmemoized caller object would otherwise tear this
    // subscription down and rebuild it on every render.
  }, [abortListing, applyEvent, client, scopeKey, transferConnectionKey]);

  // The watch set belongs to one connection and one root capability. Its
  // cleanup runs before the next sync below, so a replaced root releases every
  // watch it held before the replacement acquires anything.
  const rootToken = state.root?.token;
  useEffect(() => {
    const held = leases.current;
    return () => held.releaseAll();
  }, [rootToken, scopeKey]);

  /**
   * Exactly the directories the tree can currently reach, and therefore exactly
   * the watches it should hold. The watch bootstrap is the directory's listing,
   * so an expansion pays one round trip rather than a list and a watch.
   */
  const watchTargets = useMemo(
    () => state.root ? reachableWatchTargets(state.root.path, state.listings, state.expanded) : [],
    [state.expanded, state.listings, state.root],
  );
  // NUL cannot appear in a path, so this is an exact identity for the set,
  // and it keeps the sync below from re-running when nothing about which
  // directories are open actually moved.
  const watchTargetKey = watchTargets.join("\u0000");
  useEffect(() => {
    const activeScope = scopeRef.current;
    const root = stateRef.current.root;
    if (!activeScope || !root) return;
    leases.current.sync(watchTargetKey ? watchTargetKey.split("\u0000") : [], {
      acquire: (directory) => client.acquireDirectoryWatch(activeScope, root, directory),
      onBootstrap: (directory, listing) => applyListing(root, directory, listing),
      onError: (directory, error) => setState((current) => sameRoot(current.root, root)
        ? { ...current, loading: withoutPath(current.loading, directory), error: String(error) }
        : current),
    });
  }, [applyListing, client, scopeKey, watchTargetKey]);

  const toggleDirectory = useCallback((path: string) => {
    const expanding = !stateRef.current.expanded.has(path);
    const generation = (paintGenerations.current.get(path) ?? 0) + 1;
    paintGenerations.current.set(path, generation);
    const paint = expanding ? createPaintTicket(
      ["explorer.expandToPaint", "workflow.explorer.directoryExpandPaint"], generation,
    ) : undefined;
    const root = stateRef.current.root;
    const activeScope = scopeRef.current;
    // A valid cached revisit paints now; the watch bootstrap revalidates it.
    let painted = stateRef.current.listings.has(path);
    if (expanding && root && activeScope && !painted) {
      const cached = cache.current.get({ clientId: activeScope.clientId, rootToken: root.token, directory: path });
      if (cached) {
        recordPerfCounter("explorer.cacheHits");
        painted = true;
        applyListing(root, path, cached);
      } else {
        recordPerfCounter("explorer.cacheMisses");
      }
    }
    setState((current) => {
      const expanded = new Set(current.expanded);
      if (expanded.has(path)) expanded.delete(path);
      else expanded.add(path);
      const loading = new Set(current.loading);
      if (!expanded.has(path)) loading.delete(path);
      else if (!current.listings.has(path) && !painted) loading.add(path);
      return { ...current, expanded, loading };
    });
    if (!expanding) {
      // Collapsing stops the work its rows were asking for. The watch itself is
      // released by the sync effect, which is one unwatch for this directory.
      abortListing((candidate) => candidate === path || candidate.startsWith(`${path}/`));
    }
    if (painted || !expanding) {
      paint?.afterPaint((ticket) => ticket.lifecycleGeneration === paintGenerations.current.get(path)
        && sameRoot(stateRef.current.root, root)
        && stateRef.current.expanded.has(path));
      return;
    }
    // Nothing local to paint: the watch bootstrap this expansion triggers is
    // the listing, so the paint ticket resolves when that arrives.
    if (paint) pendingExpandPaints.current.set(path, { paint, generation });
  }, [abortListing, applyListing]);

  useEffect(() => {
    for (const [path, pending] of [...pendingExpandPaints.current]) {
      if (!state.expanded.has(path) || paintGenerations.current.get(path) !== pending.generation) {
        pendingExpandPaints.current.delete(path);
        pending.paint.abandon();
        continue;
      }
      if (!state.listings.has(path)) continue;
      pendingExpandPaints.current.delete(path);
      pending.paint.afterPaint((ticket) => ticket.lifecycleGeneration === paintGenerations.current.get(path)
        && stateRef.current.expanded.has(path));
    }
  }, [state.expanded, state.listings]);

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
    void loadDirectory(root, target).finally(() => {
      setState((current) => ({ ...current, requestedReads: Math.max(0, current.requestedReads - 1) }));
    });
  }, [loadDirectory]);

  const recordTransfer = useCallback((transfer: TransferStatus) => {
    if (transfer.scopeKey !== transferConnectionKey) return;
    setState((current) => ({ ...current, transfers: upsertTransfer(current.transfers, transfer) }));
  }, [transferConnectionKey]);

  const loadMore = useCallback((directory: string) => {
    const root = stateRef.current.root;
    if (root) void loadDirectory(root, directory, true);
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

function withoutPath(paths: ReadonlySet<string>, path: string): Set<string> {
  const next = new Set(paths);
  next.delete(path);
  return next;
}

/** Drops a deleted directory and everything the tree cached beneath it. */
function pruneSubtree(state: WorkspaceFilesState, path: string): WorkspaceFilesState {
  const prefix = `${path}/`;
  const covered = (candidate: string) => candidate === path || candidate.startsWith(prefix);
  if (![...state.listings.keys()].some(covered) && ![...state.expanded].some(covered)) return state;
  const listings = new Map(state.listings);
  const expanded = new Set(state.expanded);
  const loading = new Set(state.loading);
  for (const key of [...listings.keys()]) if (covered(key)) listings.delete(key);
  for (const key of [...expanded]) if (covered(key)) expanded.delete(key);
  for (const key of [...loading]) if (covered(key)) loading.delete(key);
  return { ...state, listings, expanded, loading };
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
