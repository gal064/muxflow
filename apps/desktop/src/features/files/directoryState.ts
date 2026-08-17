import { sameRoot } from "./api";
import { appendPage, type RecoveryReason } from "./listingModel";
import type { ActiveRoot, DirectoryListing } from "./types";

/**
 * Everything the Explorer knows about one connection's directory tree.
 *
 * Kept here, with the transitions that move it, rather than inside the hook:
 * every transition owes the same two guards — the root must still be the one
 * that authorised it, and the directory must still be one the tree is showing —
 * and those guards were written out by hand at each of them. Stated once, they
 * cannot drift, and they can be tested without mounting anything.
 */
export interface WorkspaceFilesState {
  scopeKey: string;
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
  error?: string;
}

/**
 * What a directory owes after an event its cached listing could not answer.
 *
 * `restorePages` exists because an authoritative rescan carries only the
 * directory's first page: replacing a listing the user has paged further into
 * would delete rows they can see, so the pages they had are fetched back.
 */
export type RecoveryAction =
  | { kind: "list"; reason: RecoveryReason }
  | { kind: "restorePages"; entries: number };

export const EMPTY_LISTINGS: ReadonlyMap<string, DirectoryListing> = new Map();
export const NO_RECOVERIES: ReadonlyMap<string, RecoveryAction> = new Map();

/**
 * Runs a transition against one directory, or refuses it.
 *
 * Refused when the root has moved — a path under a replaced capability is a
 * different file — or when the tree is no longer showing this directory. A
 * snapshot that races a collapse, or a recovery list whose directory was
 * deleted underneath it, must not put rows back into a tree that cannot reach
 * them.
 */
export function onDirectory(
  current: WorkspaceFilesState,
  root: ActiveRoot,
  directory: string,
  transition: (current: WorkspaceFilesState) => WorkspaceFilesState,
): WorkspaceFilesState {
  if (!sameRoot(current.root, root)) return current;
  if (!current.expanded.has(directory)) return current;
  return transition(current);
}

/**
 * Installs an authoritative listing, or records what the tree owes instead.
 *
 * Three producers write this slot — the host's native rescan, its polling
 * fallback, and client reads — and all of them mint a revision before a
 * blocking scan and publish after it, so arrival order is not freshness order.
 */
export function installListing(
  current: WorkspaceFilesState,
  directory: string,
  listing: DirectoryListing,
  options: { append?: boolean; restored?: boolean } = {},
): WorkspaceFilesState {
  const held = current.listings.get(directory);
  const loading = withoutPath(current.loading, directory);
  if (options.append) {
    // A page continues one listing, and every page of one listing reports the
    // revision that listing started with. A page whose revision no longer
    // matches is a slice of a directory that has since been re-listed: merging
    // it would put rows the rescan removed back on screen and roll the
    // listing's own revision backwards.
    if (!held || held.revision !== listing.revision) return { ...current, loading };
    const listings = new Map(current.listings).set(directory, appendPage(held, listing));
    return { ...current, listings, loading, error: undefined };
  }
  if (held && olderRevision(listing, held)) return { ...current, loading };
  // An authoritative rescan only ever carries the directory's first page.
  // Replacing a listing the user has paged further into would delete rows they
  // can see and clamp their keyboard focus to the shorter tree, so the rows
  // they have stay exactly as they are and the recovery queue re-reads the
  // whole depth before anything on screen moves.
  // `restored` is the answer to that guard rather than another instance of it:
  // a restore that could not reach the length it started from would otherwise
  // queue itself again on its own result, forever.
  if (!options.restored && held && !listing.complete && held.entries.length > listing.entries.length) {
    // A pending gap outranks a page restore — the invariant [`oweRecovery`]
    // states, which this used to run backwards. Overwriting does not merely
    // delay the gap: `restorePages` returns silently on failure or abort
    // without re-queuing anything, so an unmappable event answered by a failed
    // restore is answered by nothing at all. The restore is not lost either —
    // the recovery list installs a first page and this same guard queues it
    // then.
    if (current.recoveries.get(directory)?.kind === "list") return { ...current, loading };
    const recoveries = new Map(current.recoveries)
      .set(directory, { kind: "restorePages", entries: held.entries.length } as const);
    return { ...current, loading, recoveries };
  }
  const listings = new Map(current.listings).set(directory, listing);
  return { ...current, listings, loading, error: undefined };
}

/** Replaces one directory's listing with a locally patched one. */
export function patchListing(
  current: WorkspaceFilesState,
  directory: string,
  patched: DirectoryListing,
): WorkspaceFilesState {
  return { ...current, listings: new Map(current.listings).set(directory, patched) };
}

/**
 * Records that a directory owes a remote read.
 *
 * A pending page restore is a follow-up fetch, not an answer to a gap, so a
 * genuine gap supersedes it — never the other way round.
 */
export function oweRecovery(
  current: WorkspaceFilesState,
  directory: string,
  reason: RecoveryReason,
): WorkspaceFilesState {
  if (current.recoveries.get(directory)?.kind === "list") return current;
  const recoveries = new Map(current.recoveries).set(directory, { kind: "list", reason } as const);
  return { ...current, recoveries };
}

/** Drops a deleted directory and everything the tree cached beneath it. */
export function pruneSubtree(current: WorkspaceFilesState, path: string): WorkspaceFilesState {
  const prefix = `${path}/`;
  const covered = (candidate: string) => candidate === path || candidate.startsWith(prefix);
  if (![...current.listings.keys()].some(covered) && ![...current.expanded].some(covered)) return current;
  const listings = new Map(current.listings);
  const expanded = new Set(current.expanded);
  const loading = new Set(current.loading);
  for (const key of [...listings.keys()]) if (covered(key)) listings.delete(key);
  for (const key of [...expanded]) if (covered(key)) expanded.delete(key);
  for (const key of [...loading]) if (covered(key)) loading.delete(key);
  return { ...current, listings, expanded, loading };
}

export function withoutPath(paths: ReadonlySet<string>, path: string): Set<string> {
  const next = new Set(paths);
  next.delete(path);
  return next;
}

/**
 * Whether a listing is older than the one already held.
 *
 * Revisions are decimal u64 strings, so they are compared as numbers when both
 * parse and never compared at all when either does not — an unparseable
 * revision must not silently order as zero and discard a real listing.
 */
export function olderRevision(incoming: DirectoryListing, held: DirectoryListing): boolean {
  const next = Number(incoming.revision);
  const current = Number(held.revision);
  if (!Number.isSafeInteger(next) || !Number.isSafeInteger(current)) return false;
  return next < current;
}
