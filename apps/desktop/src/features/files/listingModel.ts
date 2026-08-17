import type { DirectoryListing, FileEntry } from "./types";

/** Why a cached listing could not answer an event on its own. */
export type RecoveryReason = "missing" | "incomplete" | "unmappable";

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
 * Whether this listing describes the whole directory, and so can be patched
 * from a precise event instead of re-listed.
 *
 * A partial page cannot: an entry belonging after the page boundary would be
 * drawn as though the tree had reached it, and one before it would be
 * indistinguishable from an entry the page simply never carried.
 */
export function isPatchable(listing: DirectoryListing | undefined): listing is DirectoryListing {
  return Boolean(listing && listing.complete && !listing.nextPageToken);
}

/** Applies one precise change, or reports why a recovery list is needed. */
export function patchEntry(
  listing: DirectoryListing | undefined,
  entry: FileEntry,
): DirectoryListing | RecoveryReason {
  if (!listing) return "missing";
  if (!isPatchable(listing)) return "incomplete";
  const index = listing.entries.findIndex((existing) => existing.path === entry.path);
  if (index >= 0) {
    const entries = [...listing.entries];
    entries[index] = entry;
    return { ...listing, entries };
  }
  const entries = [...listing.entries, entry].sort(compareEntries);
  return { ...listing, entries };
}

/** Removes one deleted path, or reports why a recovery list is needed. */
export function removeEntry(
  listing: DirectoryListing | undefined,
  path: string,
): DirectoryListing | RecoveryReason {
  if (!listing) return "missing";
  if (!isPatchable(listing)) return "incomplete";
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
