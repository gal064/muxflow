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
  /**
   * Directories that owe a remote read, and what kind.
   *
   * Recorded in state rather than acted on inside the updater that discovered
   * it: an updater must stay pure, and one directory named by twenty events in
   * one batch owes exactly one read.
   */
  recoveries: ReadonlyMap<string, RecoveryAction>;
  transfers: readonly TransferStatus[];
  error?: string;
}

/**
 * What a directory owes after an event its cached listing could not answer.
 *
 * `restorePages` exists because an authoritative rescan carries only the
 * directory's first page: replacing a listing the user has paged further into
 * would delete rows they can see, so the pages they had are fetched back.
 */
type RecoveryAction =
  | { kind: "list"; reason: RecoveryReason }
  | { kind: "restorePages"; entries: number };

const EMPTY = new Map<string, DirectoryListing>();
const NO_RECOVERIES: ReadonlyMap<string, RecoveryAction> = new Map();
/** Pages one truncated listing may fetch back before it gives up. */
const MAX_RESTORED_PAGES = 8;
const EXTERNAL_CHANGE_PAINT = ["explorer.externalChangeToPaint"] as const;
type DirectoryLoadResult = "applied" | "stale" | "failed";

/**
 * How often the active root is re-checked when nothing has announced a change.
 *
 * Every event that *can* be pushed already re-resolves it immediately; this
 * covers `cd` inside the current pane, which tmux does not announce. It is a
 * backstop rather than a pipeline: a hidden window checks nothing at all, the
 * host answers an unchanged root from the caller's own capability without a
 * second authoritative discovery or a broadcast payload, and — see
 * [`ACTIVE_ROOT_SETTLED_MULTIPLIER`] — a settled root is checked far less
 * often, though never not at all.
 */
export const ACTIVE_ROOT_BACKSTOP_MS = 15_000;

/**
 * Consecutive unchanged probes after which the backstop slows down.
 *
 * A timer that never slows is a periodic request forever, which the round's
 * idle budget refuses outright. Anything that could have moved the root — the
 * window coming back to the foreground, the host announcing a different root,
 * or the person touching the Explorer — puts it back to full rate, so the cost
 * tracks activity rather than uptime.
 */
export const ACTIVE_ROOT_STABLE_PROBES = 3;

/**
 * How much longer the settled backstop waits between probes.
 *
 * Deliberately a longer wait rather than a stop. `cd` inside the pane the user
 * is already in is announced by nothing at all, so a backstop that switches
 * itself off entirely leaves a moved root undetected for as long as the person
 * does not happen to touch the Explorer or blur the window — which on a pane
 * somebody is only reading is indefinitely.
 */
export const ACTIVE_ROOT_SETTLED_MULTIPLIER = 8;

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
    recoveries: NO_RECOVERIES,
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
  /** Re-arms the active-root backstop after it has settled. */
  const rearmBackstop = useRef<(() => void) | undefined>(undefined);
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
  const applyListing = useCallback((
    root: ActiveRoot,
    directory: string,
    listing: DirectoryListing,
    options: { append?: boolean } = {},
  ) => {
    if (listing.rootToken !== root.token) return;
    setState((current) => {
      if (!sameRoot(current.root, root)) return current;
      // Only directories the tree is actually showing. A snapshot that races a
      // collapse, or a recovery list whose directory was deleted underneath it,
      // must not put rows back into a tree that no longer reaches them.
      if (!current.expanded.has(directory)) return current;
      const held = current.listings.get(directory);
      const loading = withoutPath(current.loading, directory);
      if (options.append) {
        // A page continues one listing, and every page of one listing reports
        // the revision that listing started with. A page whose revision no
        // longer matches is a slice of a directory that has since been
        // re-listed: merging it would put rows the rescan removed back on
        // screen and roll the listing's own revision backwards.
        if (!held || held.revision !== listing.revision) return { ...current, loading };
        const listings = new Map(current.listings).set(directory, appendPage(held, listing));
        return { ...current, listings, loading, error: undefined };
      }
      // Revisions are the host's own ordering fact for one directory, and
      // three producers write this slot: the native rescan task, the polling
      // fallback, and client reads. All of them mint a revision before a
      // blocking scan and publish after it, so arrival order is not freshness
      // order and a late older snapshot would otherwise replace newer rows.
      if (held && olderRevision(listing, held)) return { ...current, loading };
      // An authoritative rescan only ever carries the directory's first page.
      // Replacing a listing the user has paged further into would delete rows
      // they can see and clamp their keyboard focus to the shorter tree, so
      // the rows they have stay exactly as they are and the recovery queue
      // re-reads the whole depth before anything on screen moves.
      if (held && !listing.complete && held.entries.length > listing.entries.length) {
        const recoveries = new Map(current.recoveries)
          .set(directory, { kind: "restorePages", entries: held.entries.length } as const);
        return { ...current, loading, recoveries };
      }
      const listings = new Map(current.listings).set(directory, listing);
      return { ...current, listings, loading, error: undefined };
    });
    if (!options.append) {
      // A listing that has been replaced outright supersedes any page read
      // still in flight for it: that page describes the directory as it was.
      directorySerial.current.set(directory, (directorySerial.current.get(directory) ?? 0) + 1);
      abortListing((candidate) => candidate === directory);
    }
  }, [abortListing]);

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
    if (!activeScope || keyForScope(activeScope) !== scopeKey) return "stale";
    if (!sameRoot(stateRef.current.root, root)) return "stale";
    // The page token is a parameter rather than something read back out of
    // state: a prefetch fires from the listing that has just arrived, before
    // React has committed it, and reading ambiently made that a silent no-op.
    const pageToken = append?.pageToken;
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
        pageToken ? "files.listDirectory.page" : "files.listDirectory",
        () => client.listDirectory(activeScope, root, path, {
          ...(pageToken ? { pageToken } : {}),
          signal: controller.signal,
        }),
      );
      if (listAborts.current.get(path) === controller) listAborts.current.delete(path);
      const superseded = epoch !== scopeEpoch.current
        || directorySerial.current.get(path) !== serial
        || keyForScope(activeScope) !== scopeKey
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
    setState((current) => {
      if (!sameRoot(current.root, root)) return current;
      // An event about a directory the tree is not showing is not a gap in
      // anything: there is no listing to patch and none is owed. Recovering
      // here would list directories the user cannot see, including the parent
      // of the root itself for an event about the root.
      if (!current.expanded.has(directory)) return current;
      const patched = patch(current.listings.get(directory));
      if (isRecoveryReason(patched)) {
        // A pending page restore is a follow-up fetch, not an answer to a gap:
        // a genuine gap must still be listed, and the list supersedes it.
        if (current.recoveries.get(directory)?.kind === "list") return current;
        const recoveries = new Map(current.recoveries);
        recoveries.set(directory, { kind: "list", reason: patched });
        return { ...current, recoveries };
      }
      const listings = new Map(current.listings);
      listings.set(directory, patched);
      return { ...current, listings };
    });
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
    const serial = (directorySerial.current.get(directory) ?? 0) + 1;
    directorySerial.current.set(directory, serial);
    let assembled: DirectoryListing | undefined;
    try {
      for (let page = 0; page < MAX_RESTORED_PAGES; page += 1) {
        const pageToken = assembled?.nextPageToken;
        if (assembled && (assembled.complete || !pageToken)) break;
        const next = await client.listDirectory(activeScope, root, directory,
          pageToken ? { pageToken } : {});
        if (epoch !== scopeEpoch.current || directorySerial.current.get(directory) !== serial) return;
        if (next.rootToken !== root.token) return;
        assembled = assembled ? appendPage(assembled, next) : next;
        if (assembled.entries.length >= entries) break;
      }
    } catch {
      // A failed restore leaves the rows the tree already had; the next
      // authoritative event asks again.
      return;
    }
    // Applied once, whole. Applying each page as it arrives would put the
    // truncation back: the tree would drop to one page and re-grow, taking the
    // keyboard focus down with it.
    if (assembled) applyListing(root, directory, assembled);
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
      if (!sameRoot(current.root, event.root)) rearmBackstop.current?.();
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
    setState((value) => pruneSubtree(value, event.path));
    abortListing((path) => path === event.path || path.startsWith(`${event.path}/`));
    applyPrecise(root, directory, (listing) => removeEntry(listing, event.path));
  }, [abortListing, applyListing, applyPrecise, transferConnectionKey]);

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
    setState((current) => current.recoveries === pending
      ? { ...current, recoveries: NO_RECOVERIES }
      : current);
  }, [coalesceRecovery, restorePages, state.recoveries, state.root]);

  useEffect(() => {
    scopeEpoch.current += 1;
    abortListing(() => true);
    if (!scope) {
      cache.current.clear();
      setState((current) => ({
        scopeKey: "", transferConnectionKey: "", listings: new Map(), expanded: new Set(), loading: new Set(),
        requestedReads: 0,
        recoveries: NO_RECOVERIES,
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
      recoveries: NO_RECOVERIES,
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
    let unchangedProbes = 0;
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
          unchangedProbes += 1;
          rootPaint.abandon();
          return;
        }
        unchangedProbes = 0;
        // A replaced root invalidates every path, listing, and content the
        // previous one authorised: the same path under a new capability is a
        // different file.
        abortListing(() => true);
        cache.current.invalidateOtherRoots(activeScope.clientId, root.token, root.revision);
        setState((current) => ({
          ...current,
          scopeKey,
          root,
          listings: new Map(),
          expanded: new Set([root.path]),
          // The root's listing arrives with its watch, and until it does the
          // tree has nothing to draw. Without this the Explorer showed no
          // rows, no wait, and no empty state for the whole first round trip.
          loading: new Set([root.path]),
          recoveries: NO_RECOVERIES,
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
    let ticks = 0;
    const backstop = window.setInterval(() => {
      if (!foreground()) return;
      ticks += 1;
      // Settled: the same check, far less often. See the multiplier's note for
      // why this slows down rather than stopping.
      const settled = unchangedProbes >= ACTIVE_ROOT_STABLE_PROBES;
      if (settled && ticks % ACTIVE_ROOT_SETTLED_MULTIPLIER !== 0) return;
      void resolve();
    }, ACTIVE_ROOT_BACKSTOP_MS);
    const rearm = () => {
      unchangedProbes = 0;
      ticks = 0;
      if (foreground()) void resolve();
    };
    rearmBackstop.current = rearm;
    const onVisibility = () => { if (foreground()) rearm(); };
    document?.addEventListener?.("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      scopeEpoch.current += 1;
      window.clearInterval(backstop);
      document?.removeEventListener?.("visibilitychange", onVisibility);
      rearmBackstop.current = undefined;
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
      directorySerial.current.clear();
      for (const pending of pendingExpandPaints.current.values()) pending.paint.abandon();
      pendingExpandPaints.current.clear();
      unsubscribe?.();
    };
    // `scope` is deliberately not a dependency: `scopeKey` is its exact
    // identity, and an unmemoized caller object would otherwise tear this
    // subscription down and rebuild it on every render.
  }, [abortListing, applyEvent, client, scopeKey, transferConnectionKey]);

  /**
   * The connection and root capability the watch set belongs to.
   *
   * Read from the *masked* view rather than raw state: on the render where the
   * scope changes, the reset has not been committed yet, and acquiring against
   * the previous root under the new scope arms and immediately releases one
   * watch per open directory on every pane switch.
   */
  const activeRoot = state.scopeKey === scopeKey ? state.root : undefined;
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
  const watchTargetKey = [scopeKey, activeRoot?.token ?? "", ...watchTargets].join("\u0000");
  useEffect(() => {
    const held = leases.current;
    return () => held.releaseAll();
  }, [activeRoot?.token, scopeKey]);
  useEffect(() => {
    const activeScope = scopeRef.current;
    if (!activeScope || !activeRoot || keyForScope(activeScope) !== scopeKey) return;
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
        recordPerfCounter("explorer.watchFallbackLists");
        void loadDirectory(root, directory);
      },
    });
    // `watchTargets` is derived from `watchTargetKey`, which is the exact
    // identity of the set; depending on the array itself would re-sync on every
    // render that rebuilt an identical list.
  }, [activeRoot, applyListing, client, loadDirectory, prefetchNextPage, scopeKey, watchTargetKey]);

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
    // Touching the tree is the strongest evidence available that the pane may
    // have moved, so it re-arms the settled root backstop.
    rearmBackstop.current?.();
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
    rearmBackstop.current?.();
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
    const held = stateRef.current.listings.get(directory);
    if (root && held?.nextPageToken && !held.complete) {
      void loadDirectory(root, directory, { pageToken: held.nextPageToken });
    }
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
    recoveries: NO_RECOVERIES,
    transfers: state.transferConnectionKey === transferConnectionKey
      ? state.transfers
      : state.transfers.map(staleTransferOnScopeReplacement),
  };
  return { ...visible, toggleDirectory, refresh, recordTransfer, loadMore };
}

/**
 * Whether a listing is older than the one already held.
 *
 * Revisions are decimal u64 strings, so they are compared as numbers when both
 * parse and never compared at all when either does not — an unparseable
 * revision must not silently order as zero and discard a real listing.
 */
function olderRevision(incoming: DirectoryListing, held: DirectoryListing): boolean {
  const next = Number(incoming.revision);
  const current = Number(held.revision);
  if (!Number.isSafeInteger(next) || !Number.isSafeInteger(current)) return false;
  return next < current;
}

/** The connection and root capability a cached listing belongs to. */
function cacheScope(scope: FileWorkspaceScope, root: ActiveRoot) {
  return { clientId: scope.clientId, rootToken: root.token, rootGeneration: root.revision };
}

function cacheKey(scope: FileWorkspaceScope, root: ActiveRoot, directory: string) {
  return { ...cacheScope(scope, root), directory };
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
