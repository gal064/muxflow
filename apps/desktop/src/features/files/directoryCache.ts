import type { DirectoryListing } from "./types";
import { isPatchable } from "./listingModel";

/** How many complete directory listings one renderer retains. */
const MAX_CACHED_DIRECTORIES = 256;

interface CacheKey {
  /** The exact live connection. A reconnect must not reuse anything. */
  clientId: string;
  /** The exact root capability. A replaced root invalidates every entry. */
  rootToken: string;
  /**
   * The root's generation, as the host reports it.
   *
   * Redundant today, and named as such rather than left looking load-bearing.
   * The host derives it as `blake3(rootToken)` (`filesystem.rs`,
   * `root_generation`), so the same token always answers with the same
   * generation and this field cannot distinguish two entries `rootToken` does
   * not already separate. It is kept because it is the identity the wire
   * carries and the host is free to make it independent later — not because it
   * is invalidating anything now. An earlier comment here claimed a capability
   * reissued under the same token carries a fresh generation. It does not, and
   * a false invariant guarding cache invalidation is worse than a redundant
   * field.
   */
  rootGeneration: string;
  directory: string;
}

/**
 * Complete directory listings, keyed by connection, root capability, and path.
 *
 * It exists so revisiting a folder paints from what the app already knows and
 * revalidates behind that paint, instead of showing an empty tree for a remote
 * round trip. Only complete listings are retained: a partial page would paint a
 * directory that looks smaller than it is.
 *
 * Every key carries the connection and the root capability, so a reconnect or a
 * root replacement can never produce a stale local paint — the entries are
 * simply unreachable, and `invalidateOtherRoots` drops them.
 */
export class DirectoryListingCache {
  readonly #entries = new Map<string, DirectoryListing>();

  get(key: CacheKey): DirectoryListing | undefined {
    const id = identity(key);
    const listing = this.#entries.get(id);
    if (!listing) return undefined;
    // Refresh recency so a working set of open folders survives a burst of
    // one-off reads elsewhere in the tree.
    this.#entries.delete(id);
    this.#entries.set(id, listing);
    return listing;
  }

  set(key: CacheKey, listing: DirectoryListing): void {
    if (!isPatchable(listing) || listing.rootToken !== key.rootToken) return;
    const id = identity(key);
    this.#entries.delete(id);
    this.#entries.set(id, listing);
    while (this.#entries.size > MAX_CACHED_DIRECTORIES) {
      const oldest = this.#entries.keys().next();
      if (oldest.done) break;
      this.#entries.delete(oldest.value);
    }
  }

  /**
   * Drops a deleted directory and everything beneath it.
   *
   * Without this, a directory removed and later recreated at the same path
   * would paint the contents it had in a previous life: the key — connection,
   * root capability, path — is unchanged, and the cache exists precisely to
   * paint before revalidation.
   */
  invalidateSubtree(scope: Omit<CacheKey, "directory">, path: string): void {
    const removed = identity({ ...scope, directory: path });
    for (const held of [...this.#entries.keys()]) {
      if (held === removed || held.startsWith(`${removed}/`)) this.#entries.delete(held);
    }
  }

  /** Drops everything that is not the exact live connection and root. */
  invalidateOtherRoots(clientId: string, rootToken: string, rootGeneration: string): void {
    const prefix = `${clientId}\u0000${rootToken}\u0000${rootGeneration}\u0000`;
    for (const key of [...this.#entries.keys()]) {
      if (!key.startsWith(prefix)) this.#entries.delete(key);
    }
  }

  clear(): void {
    this.#entries.clear();
  }

  get size(): number {
    return this.#entries.size;
  }
}

/** NUL cannot appear in a path, so no two distinct keys can collide. */
function identity(key: CacheKey): string {
  return `${key.clientId}\u0000${key.rootToken}\u0000${key.rootGeneration}\u0000${key.directory}`;
}
