import type { DirectoryListing, FileEntry } from "./types";

/** Why a cached listing could not answer an event on its own. */
export type RecoveryReason = "missing" | "unmappable";

export function parentPath(path: string): string {
  const index = path.lastIndexOf("/");
  return index <= 0 ? "/" : path.slice(0, index);
}

/**
 * The host's own entry order: directories first, then name.
 *
 * It ranks on the raw name bytes, which a renderer cannot see; comparing the
 * mapped strings agrees for every name that survives UTF-8 and differs only in
 * the position of a name that does not. That is a cosmetic ordering difference
 * in a locally patched listing, and the next authoritative snapshot replaces it
 * outright.
 */
export function compareEntries(left: FileEntry, right: FileEntry): number {
  const leftRank = left.kind === "directory" ? 0 : 1;
  const rightRank = right.kind === "directory" ? 0 : 1;
  if (leftRank !== rightRank) return leftRank - rightRank;
  return left.name < right.name ? -1 : left.name > right.name ? 1 : 0;
}

/**
 * Whether this listing describes the whole directory.
 *
 * Only a complete listing is worth caching for a later revisit — a partial one
 * would paint a directory the tree has not finished reading. Patching does
 * *not* require it: see [`covers`].
 */
export function isPatchable(listing: DirectoryListing | undefined): listing is DirectoryListing {
  return Boolean(listing && listing.complete && !listing.nextPageToken);
}

/**
 * Whether this listing's rows are entitled to an opinion about `entry`.
 *
 * Pages are a contiguous prefix of the host's order, so an incomplete listing
 * holds everything up to its last row and nothing after it. A change at or
 * before that boundary belongs on screen; one after it belongs to a page the
 * tree has not asked for, and is not a gap in anything.
 *
 * Requiring a *complete* listing here was the single most expensive rule in
 * the feature: the host's page is 4,096 entries, so in any directory larger
 * than that every single-file change fell through to a recovery list and then
 * to a full re-pagination — the exact list storm this package exists to
 * remove, in precisely the directories where it costs most.
 */
function covers(listing: DirectoryListing, entry: FileEntry): boolean {
  if (listing.complete && !listing.nextPageToken) return true;
  const last = listing.entries.at(-1);
  return last !== undefined && compareEntries(entry, last) <= 0;
}

/** Applies one precise change, or reports why a recovery list is needed. */
export function patchEntry(
  listing: DirectoryListing | undefined,
  entry: FileEntry,
): DirectoryListing | RecoveryReason {
  if (!listing) return "missing";
  const index = listing.entries.findIndex((existing) => existing.path === entry.path);
  if (index >= 0) {
    const entries = [...listing.entries];
    entries[index] = entry;
    return { ...listing, entries };
  }
  // Beyond the rows this listing holds: the entry lives in a page nobody has
  // asked for, so there is nothing on screen to correct.
  if (!covers(listing, entry)) return listing;
  const entries = [...listing.entries, entry].sort(compareEntries);
  return { ...listing, entries };
}

/** Removes one deleted path, or reports why a recovery list is needed. */
export function removeEntry(
  listing: DirectoryListing | undefined,
  path: string,
): DirectoryListing | RecoveryReason {
  if (!listing) return "missing";
  // A path this listing never held needs no removal, whether that is because
  // the directory does not contain it or because it sits in a later page.
  if (!listing.entries.some((entry) => entry.path === path)) return listing;
  return { ...listing, entries: listing.entries.filter((entry) => entry.path !== path) };
}

export function isRecoveryReason(value: DirectoryListing | RecoveryReason): value is RecoveryReason {
  return typeof value === "string";
}

/** Appends a later page to an established listing without losing earlier rows. */
export function appendPage(previous: DirectoryListing, page: DirectoryListing): DirectoryListing {
  const byPath = new Map(previous.entries.map((entry) => [entry.path, entry]));
  for (const entry of page.entries) byPath.set(entry.path, entry);
  return {
    ...page,
    entries: [...byPath.values()],
    recoveredFromOverflow: previous.recoveredFromOverflow || page.recoveredFromOverflow,
  };
}

/**
 * The directories a watch is actually owed: the root, plus every expanded
 * directory the tree can currently reach through expanded, expandable parents.
 *
 * Expansion state is deliberately kept for directories that are *not* reachable
 * — collapsing a parent must not forget which of its children were open — but
 * holding remote watches on rows nobody can see is a per-directory host
 * watcher, descriptor, and event stream paid for nothing.
 */
export function reachableWatchTargets(
  rootPath: string,
  listings: ReadonlyMap<string, DirectoryListing>,
  expanded: ReadonlySet<string>,
): string[] {
  const targets = [rootPath];
  // Visited, because the walk descends into whatever the host said a directory
  // contains. A listing that named an ancestor — a link resolved to one, a
  // malformed entry — would otherwise be an unbounded descent rather than a
  // wrong row.
  const seen = new Set([rootPath]);
  const visit = (directory: string) => {
    const listing = listings.get(directory);
    for (const entry of listing?.entries ?? []) {
      if (!entry.expandable || !expanded.has(entry.path) || seen.has(entry.path)) continue;
      seen.add(entry.path);
      targets.push(entry.path);
      visit(entry.path);
    }
  };
  visit(rootPath);
  return targets;
}
