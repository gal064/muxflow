import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { keyForScope, sameRoot } from "./api";
import { measurePerfOutcome, recordPerfCounter } from "../../perf/probe";
import { createPaintTicket, type PaintTicket } from "../../perf/paintTicket";
import { DirectoryListingCache } from "./directoryCache";
import { DirectoryRequests } from "./directoryRequests";
import {
  appendPage,
  isRecoveryReason,
  parentPath,
  patchEntry,
  reachableWatchTargets,
  removeEntry,
  type RecoveryReason,
} from "./listingModel";
import {
  EMPTY_LISTINGS,
  NO_RECOVERIES,
  installListing,
  onDirectory,
  consumeRecoveries,
  oweRecovery,
  patchListing,
  pruneSubtree,
  withoutPath,
  type WorkspaceFilesState,
} from "./directoryState";
import { useCommittedRef } from "../../commands/useCommittedRef";
import { useActiveRoot } from "./useActiveRoot";
import { useConnectionTransfers } from "./useConnectionTransfers";
import { DirectoryWatchLeases } from "./watchLeases";
import type {
  ActiveRoot,
  DirectoryListing,
  FileWorkspaceClient,
  FileWorkspaceScope,
  WorkspaceEvent,
} from "./types";

/** Pages one truncated listing may fetch back before it gives up. */
const MAX_RESTORED_PAGES = 8;
const EXTERNAL_CHANGE_PAINT = ["explorer.externalChangeToPaint"] as const;

export {
  ACTIVE_ROOT_BACKSTOP_MS,
  ACTIVE_ROOT_SETTLED_MULTIPLIER,
  ACTIVE_ROOT_STABLE_PROBES,
} from "./useActiveRoot";
type DirectoryLoadResult = "applied" | "stale" | "failed";

/**
 * How long one directory's *recovery* is deferred before it is re-read.
 *
 * Only events a cached listing could not answer reach this. Precise events with
 * a mapped entry patch the listing in place and cost no request at all, so this
 * window now throttles genuine gaps — overflow, an incomplete page, an entry
 * the host could not map — rather than ordinary file writes.
 */
export const DIRECTORY_REFRESH_COALESCE_MS = 150;

export function useWorkspaceFiles(
  client: FileWorkspaceClient,
  scope: FileWorkspaceScope | undefined,
  selectionKey = scope ? keyForScope(scope) : "",
) {
  const [state, setState] = useState<WorkspaceFilesState>({
    scopeKey: "",
    listings: EMPTY_LISTINGS,
    expanded: new Set(),
    loading: new Set(),
    requestedReads: 0,
    recoveries: NO_RECOVERIES,
  });
  const downloads = useConnectionTransfers(scope);
  const recordTransfer = downloads.record;
  // Committed rather than written during render: every reader below is a
  // completion guard or a timer running after the fact, and a render React
  // discarded is not a state any of them should be deciding against.
  const stateRef = useCommittedRef(state);
  const scopeEpoch = useRef(0);
  const requests = useRef(new DirectoryRequests());
  const refreshTimers = useRef(new Map<string, { timer: ReturnType<typeof setTimeout>; paint: PaintTicket }>());
  const paintGenerations = useRef(new Map<string, number>());
  /** Expansions whose paint is owed by a watch bootstrap that has not landed. */
  const pendingExpandPaints = useRef(new Map<string, { paint: PaintTicket; generation: number }>());
  /** The active-root backstop's re-arm, held so callers declared above it can use it. */
  const rearmRoot = useRef<(() => void) | undefined>(undefined);
  const noteRootActivity = useRef<(() => void) | undefined>(undefined);
  const cache = useRef(new DirectoryListingCache());
  const leases = useRef(new DirectoryWatchLeases());
  const liveScopeKey = scope ? keyForScope(scope) : "";
  const scopeRef = useCommittedRef(scope);

  /**
   * Stops remote read work for directories nothing will read any more.
   *
   * Both halves, because a read has two states and only one of them is in
   * flight. Aborting the controllers alone left a recovery still waiting out
   * its 150 ms coalescing window, which then issued a full `listDirectory` for
   * a directory the tree had already collapsed — "collapse is one unwatch",
   * plus sometimes one list.
   */
  const abortListing = useCallback((predicate: (path: string) => boolean) => {
    requests.current.abort(predicate);
    for (const [key, pending] of [...refreshTimers.current]) {
      // The key is `${rootToken}\0${path}`; the predicate is about paths.
      const path = key.slice(key.indexOf("\0") + 1);
      if (!predicate(path)) continue;
      clearTimeout(pending.timer);
      pending.paint.abandon();
      refreshTimers.current.delete(key);
    }
  }, []);

  /**
   * Installs an authoritative listing for one directory.
   *
   * The one place a listing enters the tree, so caching, loading state, and the
   * root-token guard cannot drift apart between the watch bootstrap, an
   * authoritative rescan, and a recovery list.
   */
  const applyListing = useCallback((
    root: ActiveRoot,
    directory: string,
    listing: DirectoryListing,
    options: { append?: boolean; restored?: boolean } = {},
  ) => {
    if (listing.rootToken !== root.token) {
      // Not this root's listing, and not a silent no-op either: the row that
      // was waiting for it still owes the user its wait state back, or
      // `aria-busy` stays set on an empty folder with no error, forever.
      setState((current) => ({ ...current, loading: withoutPath(current.loading, directory) }));
      return;
    }
    setState((current) => {
      // The wait state comes back whatever the transition decides, settled here
      // rather than inside each of the transition's branches because every exit
      // owes it. `onDirectory` refuses a directory the tree is no longer
      // showing — a listing still in flight when the user collapsed the folder
      // — by returning the state unchanged, which left that path in `loading`
      // for the life of the scope and `aria-busy` set on the whole Explorer
      // with nothing on screen to explain it.
      const settled = current.loading.has(directory)
        ? { ...current, loading: withoutPath(current.loading, directory) }
        : current;
      return onDirectory(settled, root, directory,
        (held) => installListing(held, directory, listing, options));
    });
    if (!options.append) {
      // A listing that has been replaced outright supersedes any read still in
      // flight for it: that read describes the directory as it was.
      requests.current.supersede(directory);
    }
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
    append?: { pageToken: string },
  ): Promise<DirectoryLoadResult> => {
    const activeScope = scopeRef.current;
    if (!activeScope || keyForScope(activeScope) !== liveScopeKey) return "stale";
    if (!sameRoot(stateRef.current.root, root)) return "stale";
    // The page token is a parameter rather than something read back out of
    // state: a prefetch fires from the listing that has just arrived, before
    // React has committed it, and reading ambiently made that a silent no-op.
    const pageToken = append?.pageToken;
    const epoch = scopeEpoch.current;
    const slot = requests.current.open(path, pageToken ? "page" : "list");
    setState((current) => ({ ...current, loading: new Set(current.loading).add(path), error: undefined }));
    try {
      // What a directory read costs on this link, when `ADE_PERF_LOG` is set and
      // nothing otherwise. The flicker this hook was reported for is only ever
      // visible when this number is large, and until now nothing measured it.
      const listing = await measurePerfOutcome(
        pageToken ? "files.listDirectory.page" : "files.listDirectory",
        () => client.listDirectory(activeScope, root, path, {
          ...(pageToken ? { pageToken } : {}),
          signal: slot.signal,
        }),
      );
      slot.close();
      const superseded = epoch !== scopeEpoch.current
        || !slot.current()
        || keyForScope(activeScope) !== liveScopeKey
        || !sameRoot(stateRef.current.root, root)
        || listing.rootToken !== root.token;
      if (superseded) {
        // A read whose answer is no longer wanted still owes the row its wait
        // state back; `aria-busy` would otherwise stay set forever.
        setState((current) => {
          if (!current.loading.has(path)) return current;
          return { ...current, loading: withoutPath(current.loading, path) };
        });
        return "stale";
      }
      // Merged inside the state transition, against whatever the tree holds
      // now. Merging onto the listing captured before the round trip
      // reinstated rows a rescan or a delete had removed while the page was in
      // flight — and then cached them.
      applyListing(root, path, listing, { append: Boolean(pageToken) });
      return "applied";
    } catch (error) {
      slot.close();
      const aborted = slot.signal.aborted;
      setState((current) => {
        const loading = new Set(current.loading);
        loading.delete(path);
        if (!sameRoot(current.root, root)) return current;
        return aborted ? { ...current, loading } : { ...current, loading, error: String(error) };
      });
      return aborted ? "stale" : "failed";
    }
  }, [applyListing, client, liveScopeKey]);

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
   * The patch is computed *inside* the state updater, against whatever listing
   * the tree actually holds at that moment. Computing it outside, from a ref
   * refreshed only on render, meant two events for one directory delivered in
   * the same batch both patched the same base and the second silently dropped
   * the first's row — the host frame reader flushes a queue synchronously, so
   * that batch is the ordinary case, not a rare one.
   *
   * A listing that cannot represent the change records a recovery reason in
   * state rather than issuing a request from inside the updater, which keeps
   * the updater pure and the decision exactly once per directory per batch.
   */
  const applyPrecise = useCallback((
    root: ActiveRoot,
    directory: string,
    patch: (listing: DirectoryListing | undefined) => DirectoryListing | RecoveryReason,
  ) => {
    const paint = createPaintTicket(EXTERNAL_CHANGE_PAINT, scopeEpoch.current);
    const before = stateRef.current.listings.get(directory);
    // An event about a directory the tree is not showing is not a gap in
    // anything: there is no listing to patch and none is owed. Recovering here
    // would list directories the user cannot see, including the parent of the
    // root itself for an event about the root.
    setState((current) => onDirectory(current, root, directory, (held) => {
      const patched = patch(held.listings.get(directory));
      return isRecoveryReason(patched)
        ? oweRecovery(held, directory, patched)
        : patchListing(held, directory, patched);
    }));
    // Committed state decides whether anything actually moved, so a no-op
    // patch neither publishes a paint measurement nor counts as one.
    paint.afterPaint((ticket) => ticket.lifecycleGeneration === scopeEpoch.current
      && sameRoot(stateRef.current.root, root)
      && stateRef.current.expanded.has(directory)
      && stateRef.current.listings.get(directory) !== before,
    () => recordPerfCounter("explorer.listingPatches"));
  }, []);

  /**
   * Reads one bounded, high-confidence target ahead of being asked for it.
   *
   * Exactly two things qualify, and both are already in hand when a directory
   * opens: the selected workspace root and a just-opened directory's first
   * page arrive with their watch bootstrap, so neither costs a speculative
   * request. What remains is the *second* page of a directory whose first page
   * did not finish it — one page, never more, and abandoned outright if the
   * connection, root, or the directory's own place in the tree changes before
   * it lands. Speculating any further would be the list storm this whole change
   * exists to remove.
   */
  const prefetchNextPage = useCallback((root: ActiveRoot, directory: string, listing: DirectoryListing) => {
    if (listing.complete || !listing.nextPageToken) return;
    recordPerfCounter("explorer.prefetchedPages");
    void loadDirectory(root, directory, { pageToken: listing.nextPageToken });
  }, [loadDirectory]);

  /**
   * Fetches back the pages an authoritative first-page rescan replaced.
   *
   * Bounded twice over: it stops as soon as the listing is at least as long as
   * it was, and never asks for more than [`MAX_RESTORED_PAGES`] pages.
   */
  const restorePages = useCallback(async (root: ActiveRoot, directory: string, entries: number) => {
    const activeScope = scopeRef.current;
    if (!activeScope) return;
    const epoch = scopeEpoch.current;
    // A read like any other: registered, cancellable, and visible as a wait.
    // Left outside the ledger it was the one list path a collapse, a root
    // replacement, or a closing pane could not stop — up to eight sequential
    // remote round trips for a directory nobody was looking at any more.
    const slot = requests.current.open(directory, "restore");
    setState((current) => ({ ...current, loading: new Set(current.loading).add(directory) }));
    let assembled: DirectoryListing | undefined;
    try {
      for (let page = 0; page < MAX_RESTORED_PAGES; page += 1) {
        const pageToken = assembled?.nextPageToken;
        if (assembled && (assembled.complete || !pageToken)) break;
        const next = await client.listDirectory(activeScope, root, directory, {
          ...(pageToken ? { pageToken } : {}),
          signal: slot.signal,
        });
        if (epoch !== scopeEpoch.current || !slot.current()) return;
        if (next.rootToken !== root.token) return;
        assembled = assembled ? appendPage(assembled, next) : next;
        if (assembled.entries.length >= entries) break;
      }
    } catch {
      // A failed or abandoned restore leaves the rows the tree already had;
      // the next authoritative event asks again.
      return;
    } finally {
      slot.close();
      setState((current) => ({ ...current, loading: withoutPath(current.loading, directory) }));
    }
    // Applied once, whole, and marked as the restore it is. Applying each page
    // as it arrives would put the truncation back — the tree would drop to one
    // page and re-grow, taking the keyboard focus with it — and applying it
    // unmarked would let a restore that could not reach the length it started
    // from queue *itself* again, forever.
    if (assembled) applyListing(root, directory, assembled, { restored: true });
  }, [applyListing, client]);

  const applyEvent = useCallback((event: WorkspaceEvent) => {
    const current = stateRef.current;
    if (event.kind === "rootChanged") {
      // A trigger, never an answer. Root responses are returned directly to the
      // poller, where the scope epoch and operation serial reject completions
      // from the previous pane; this broadcast carries no caller epoch, so
      // installing its root would reintroduce exactly the stale cross-pane race
      // those barriers exist to prevent. Announcing that the root may have
      // moved is still real information, so it re-arms the backstop and the
      // guarded probe decides.
      if (!sameRoot(current.root, event.root)) rearmRoot.current?.();
      return;
    }
    if (event.kind === "transfer") {
      recordTransfer(event.transfer);
      return;
    }
    const root = current.root;
    if (!root || event.rootToken !== root.token) return;
    if (event.kind === "directorySnapshot") {
      // Authoritative: the host re-listed and this *is* the directory now.
      recordPerfCounter("explorer.authoritativeSnapshots");
      if (event.listing.recoveredFromOverflow) {
        // The watcher lost events, so what it lost may have been below this
        // directory as well. Cached listings for the subtree are no longer
        // safe to paint from; the ones on screen revalidate through their own
        // watches, and the ones that are not stop being a local answer.
        recordPerfCounter("explorer.overflowRecoveries");
        const activeScope = scopeRef.current;
        if (activeScope) {
          cache.current.invalidateSubtree(cacheScope(activeScope, root), event.listing.directory);
        }
      }
      applyListing(root, event.listing.directory, event.listing);
      return;
    }
    const directory = parentPath(event.path);
    if (event.kind === "fileChanged") {
      const entry = event.entry;
      applyPrecise(root, directory, (listing) => entry ? patchEntry(listing, entry) : "unmappable");
      return;
    }
    // A deleted directory takes its whole cached subtree with it, locally as
    // well as on screen: a path recreated later is a different directory and
    // must never paint from what the old one held.
    const activeScope = scopeRef.current;
    if (activeScope) cache.current.invalidateSubtree(cacheScope(activeScope, root), event.path);
    // Except the root, which is not a row in anything and whose expansion is
    // what makes the tree exist at all. Pruning it left a tree that refused
    // every subsequent transition — including the listing that would have
    // shown the directory coming back — until the root itself was re-resolved.
    if (event.path !== root.path) setState((value) => pruneSubtree(value, event.path));
    abortListing((path) => path === event.path || path.startsWith(`${event.path}/`));
    applyPrecise(root, directory, (listing) => removeEntry(listing, event.path));
  }, [abortListing, applyListing, applyPrecise, recordTransfer]);

  // Remote reads are issued here rather than from inside a state updater, so
  // one directory owes at most one read however many events named it.
  useEffect(() => {
    if (state.recoveries.size === 0) return;
    const root = state.root;
    const pending = state.recoveries;
    if (root) for (const [directory, action] of pending) {
      if (action.kind === "list") coalesceRecovery(root, directory, action.reason);
      else void restorePages(root, directory, action.entries);
    }
    // Per entry, by identity: an event raised between the render that produced
    // `pending` and this update must not make the whole queue look undrained.
    setState((current) => consumeRecoveries(current, pending));
  }, [coalesceRecovery, restorePages, state.recoveries, state.root]);

  // Logical navigation owns what may remain visible. A transport reconnect
  // replaces the live scope below, but the same host/server/session/pane keeps
  // its last authoritative tree on screen while writes are frozen.
  useEffect(() => {
    if (stateRef.current.scopeKey === selectionKey) return;
    scopeEpoch.current += 1;
    abortListing(() => true);
    setState({
      scopeKey: selectionKey,
      listings: new Map(),
      expanded: new Set(),
      loading: new Set(),
      requestedReads: 0,
      recoveries: NO_RECOVERIES,
    });
  }, [abortListing, selectionKey, stateRef]);

  // Transport ownership is shorter-lived than the visible workspace. Stop
  // every request/watch from the retiring epoch, retain the painted tree, and
  // let the active-root resolver revalidate it when the `connected` scope
  // appears. In particular, no request is issued from the snapshot-before-
  // ready window during bridge startup.
  useEffect(() => {
    scopeEpoch.current += 1;
    abortListing(() => true);
    if (!scope) {
      setState((current) => current.scopeKey === selectionKey ? {
        ...current,
        loading: new Set(),
        requestedReads: 0,
        recoveries: NO_RECOVERIES,
        error: undefined,
      } : current);
      return;
    }
    setState((current) => current.scopeKey === selectionKey ? {
      ...current,
      loading: new Set(),
      requestedReads: 0,
      recoveries: NO_RECOVERIES,
      error: undefined,
    } : current);
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void client.subscribe(scope, applyEvent).then((stop) => {
      if (disposed) stop();
      else unsubscribe = stop;
    }).catch((error) => { if (!disposed) setState((value) => ({ ...value, error: String(error) })); });
    return () => {
      disposed = true;
      scopeEpoch.current += 1;
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
      requests.current.clear();
      for (const pending of pendingExpandPaints.current.values()) pending.paint.abandon();
      pendingExpandPaints.current.clear();
      unsubscribe?.();
    };
    // `scope` is deliberately not a dependency: `liveScopeKey` is its exact
    // identity, and an unmemoized caller object would otherwise tear this
    // subscription down and rebuild it on every render.
  }, [abortListing, applyEvent, client, liveScopeKey, selectionKey]);

  /**
   * Installs a root that is genuinely different from the one on screen.
   *
   * A replaced root invalidates every path, listing, and content the previous
   * one authorised: the same path under a new capability is a different file.
   */
  const adoptRoot = useCallback((root: ActiveRoot) => {
    const activeScope = scopeRef.current;
    if (!activeScope) return;
    abortListing(() => true);
    cache.current.invalidateOtherRoots(activeScope.clientId, root.token, root.revision);
    setState((current) => ({
      ...current,
      scopeKey: selectionKey,
      root,
      listings: new Map(),
      expanded: new Set([root.path]),
      // The root's listing arrives with its watch, and until it does the tree
      // has nothing to draw. Without this the Explorer showed no rows, no
      // wait, and no empty state for the whole first round trip.
      loading: new Set([root.path]),
      recoveries: NO_RECOVERIES,
      error: undefined,
    }));
  }, [abortListing, selectionKey]);

  const { rearm, noteActivity } = useActiveRoot({
    client,
    scope: () => scopeRef.current,
    scopeKey: liveScopeKey,
    held: () => stateRef.current.root,
    onRoot: adoptRoot,
    onError: (message) => setState((current) => ({ ...current, error: message })),
    lifecycle: () => scopeEpoch.current,
  });
  useEffect(() => { rearmRoot.current = rearm; }, [rearm]);
  useEffect(() => { noteRootActivity.current = noteActivity; }, [noteActivity]);
  /**
   * The connection and root capability the watch set belongs to.
   *
   * Read from the *masked* view rather than raw state: on the render where the
   * scope changes, the reset has not been committed yet, and acquiring against
   * the previous root under the new scope arms and immediately releases one
   * watch per open directory on every pane switch.
   */
  const activeRoot = state.scopeKey === selectionKey ? state.root : undefined;
  /**
   * Exactly the directories the tree can currently reach, and therefore exactly
   * the watches it should hold. The watch bootstrap is the directory's listing,
   * so an expansion pays one round trip rather than a list and a watch.
   */
  const watchTargets = useMemo(
    () => activeRoot ? reachableWatchTargets(activeRoot.path, state.listings, state.expanded) : [],
    [activeRoot, state.expanded, state.listings],
  );
  // NUL cannot appear in a path, so this is an exact identity for the whole
  // lease set — connection, root capability, and directories. Every part is in
  // the key deliberately: release and acquire must key on the same identity, or
  // a root replaced at the same path releases every watch and re-acquires none.
  const watchTargetKey = [liveScopeKey, activeRoot?.token ?? "", ...watchTargets].join("\u0000");
  useEffect(() => {
    const held = leases.current;
    return () => held.releaseAll();
  }, [activeRoot?.token, liveScopeKey]);
  useEffect(() => {
    const activeScope = scopeRef.current;
    if (!activeScope || !activeRoot || keyForScope(activeScope) !== liveScopeKey) return;
    const root = activeRoot;
    leases.current.sync(watchTargets, {
      acquire: (directory, signal) => client.acquireDirectoryWatch(activeScope, root, directory, { signal }),
      onBootstrap: (directory, listing, fresh) => {
        if (!fresh) {
          // A watch another surface had already armed — an open file's tab
          // watches its own parent — answers with the listing it produced when
          // it was armed, which can be arbitrarily old. Painting it is fine and
          // is the whole point of having it; trusting it is not, so this is the
          // one bootstrap that still owes a read.
          recordPerfCounter("explorer.joinedWatchRevalidations");
          if (!stateRef.current.listings.has(directory)) applyListing(root, directory, listing);
          void loadDirectory(root, directory);
          return;
        }
        applyListing(root, directory, listing);
        prefetchNextPage(root, directory, listing);
      },
      // A watch we could not arm must not also mean a directory with no
      // contents: the bootstrap is the listing, so without this one refused
      // registration — the host's watch limit, an exhausted inotify budget —
      // leaves that directory, or the whole tree, permanently empty.
      onError: (directory, error) => {
        // An abandoned expansion is not a failure to report or to list again.
        if (error instanceof DOMException && error.name === "AbortError") return;
        recordPerfCounter("explorer.watchFallbackLists");
        void loadDirectory(root, directory).then((result) => {
          if (result !== "failed") return;
          setState((current) => sameRoot(current.root, root)
            ? { ...current, loading: withoutPath(current.loading, directory), error: String(error) }
            : current);
        });
      },
      // A directory still inside a refused watch's backoff window gets no
      // watch this pass. It still has to get its contents: otherwise expanding
      // it shows an empty folder marked busy, and nothing ever clears either.
      onDeferred: (directory) => {
        if (stateRef.current.listings.has(directory)) return;
        // And not while its own fallback list is still fetching. `sync` runs on
        // every change to the watch-target set — every expand and every
        // collapse anywhere in the tree — so without this the backoff stops the
        // *watch* being re-requested and nothing stops the *list*: each
        // interaction re-entered here, `open(directory, "list")` aborted the
        // read already in flight, and a refused directory could be starved
        // indefinitely while paying for a cancelled round trip per keystroke.
        // The queried owner rather than the mirrored `loading` set, because the
        // fact belongs to `DirectoryRequests` and the mirror can lag it.
        if (requests.current.reading(directory, "list")) return;
        recordPerfCounter("explorer.watchFallbackLists");
        void loadDirectory(root, directory);
      },
    });
    // `watchTargets` is derived from `watchTargetKey`, which is the exact
    // identity of the set; depending on the array itself would re-sync on every
    // render that rebuilt an identical list.
  }, [activeRoot, applyListing, client, liveScopeKey, loadDirectory, prefetchNextPage, watchTargetKey]);

  /**
   * One owner for what the cache holds.
   *
   * Mirroring committed state means a listing enters the cache by exactly the
   * same rule however it arrived — bootstrap, authoritative rescan, precise
   * patch, recovery list, or restored page — and `set` itself refuses anything
   * incomplete or bound to another root.
   *
   * Only directories the tree can currently reach are mirrored, which is
   * exactly the set that holds a watch. An expanded directory whose parent has
   * since collapsed receives no events at all, so caching it would preserve a
   * listing that nothing will ever correct — and would put back, on the very
   * next commit, whatever an overflow recovery had just dropped.
   */
  useEffect(() => {
    const activeScope = scopeRef.current;
    if (!activeScope || !activeRoot) return;
    for (const directory of watchTargets) {
      const listing = state.listings.get(directory);
      if (listing) cache.current.set(cacheKey(activeScope, activeRoot, directory), listing);
    }
  }, [activeRoot, state.listings, watchTargetKey]);

  const toggleDirectory = useCallback((path: string) => {
    const expanding = !stateRef.current.expanded.has(path);
    const generation = (paintGenerations.current.get(path) ?? 0) + 1;
    paintGenerations.current.set(path, generation);
    const paint = expanding ? createPaintTicket(
      ["explorer.expandToPaint", "workflow.explorer.directoryExpandPaint"], generation,
    ) : undefined;
    // Activity, not evidence. Working in the tree means the settled backstop
    // should go back to full rate — but expanding a folder says nothing about
    // whether the pane's `cd` moved, and probing for it here put a
    // `resolveActiveRoot` (a `tmux` fork on the host) on every expand *and*
    // every collapse, which is the interaction path this package exists to
    // make cheap.
    noteRootActivity.current?.();
    const root = stateRef.current.root;
    const activeScope = scopeRef.current;
    // A valid cached revisit and the expansion itself are one state
    // transition. Applying the listing separately meant it was applied against
    // a state where this directory was not expanded yet — and every path into
    // the listings map refuses a directory the tree is not showing — so the
    // cached rows were silently dropped and the "paint locally, revalidate
    // behind it" promise was never actually kept.
    const cached = expanding && root && activeScope && !stateRef.current.listings.has(path)
      ? cache.current.get(cacheKey(activeScope, root, path))
      : undefined;
    if (expanding && root && activeScope && !stateRef.current.listings.has(path)) {
      recordPerfCounter(cached ? "explorer.cacheHits" : "explorer.cacheMisses");
    }
    setState((current) => {
      const expanded = new Set(current.expanded);
      if (expanded.has(path)) expanded.delete(path);
      else expanded.add(path);
      const listings = cached && expanded.has(path) && sameRoot(current.root, root)
        ? new Map(current.listings).set(path, cached)
        : current.listings;
      const loading = new Set(current.loading);
      if (!expanded.has(path)) loading.delete(path);
      else if (!listings.has(path)) loading.add(path);
      return { ...current, expanded, listings, loading };
    });
    if (!expanding) {
      // Collapsing stops the work its rows were asking for. The watch itself is
      // released by the sync effect, which is one unwatch for this directory.
      abortListing((candidate) => candidate === path || candidate.startsWith(`${path}/`));
      paint?.abandon();
      return;
    }
    // The paint is owed by whatever puts rows on screen: the cached listing
    // above, or the watch bootstrap this expansion triggers. The predicate
    // requires the listing rather than the expansion flag, so an expansion that
    // painted nothing cannot publish an instant expand-to-paint measurement.
    if (paint) pendingExpandPaints.current.set(path, { paint, generation });
  }, [abortListing]);

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
        && stateRef.current.expanded.has(path)
        && stateRef.current.listings.has(path));
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
    rearmRoot.current?.();
    setState((current) => ({ ...current, requestedReads: current.requestedReads + 1 }));
    void loadDirectory(root, target).finally(() => {
      setState((current) => ({ ...current, requestedReads: Math.max(0, current.requestedReads - 1) }));
    });
  }, [loadDirectory]);

  const loadMore = useCallback((directory: string) => {
    const root = stateRef.current.root;
    const held = stateRef.current.listings.get(directory);
    if (root && held?.nextPageToken && !held.complete) {
      void loadDirectory(root, directory, { pageToken: held.nextPageToken });
    }
  }, [loadDirectory]);

  // Effects run after paint. Mask the prior pane synchronously on the render
  // where the logical selection changes so Explorer never flashes or acts on
  // another pane's old root. A live-scope-only change deliberately retains it.
  const visible = state.scopeKey === selectionKey ? state : {
    scopeKey: selectionKey,
    listings: EMPTY_LISTINGS,
    expanded: new Set<string>(),
    loading: new Set<string>(),
    requestedReads: 0,
    recoveries: NO_RECOVERIES,
  };
  return { ...visible, transfers: downloads.transfers, toggleDirectory, refresh, recordTransfer, loadMore };
}

/** The connection and root capability a cached listing belongs to. */
function cacheScope(scope: FileWorkspaceScope, root: ActiveRoot) {
  return { clientId: scope.clientId, rootToken: root.token, rootGeneration: root.revision };
}

function cacheKey(scope: FileWorkspaceScope, root: ActiveRoot, directory: string) {
  return { ...cacheScope(scope, root), directory };
}
