import type { DirectoryListing, DirectoryWatchLease } from "./types";

/** First wait after a refused watch, doubled on each further refusal. */
export const WATCH_RETRY_BASE_MS = 1_000;
export const WATCH_RETRY_MAX_MS = 30_000;

export interface WatchLeaseHost {
  acquire(directory: string, signal: AbortSignal): Promise<DirectoryWatchLease>;
  /** The bootstrap listing the host returned when the watch was armed. */
  onBootstrap(directory: string, listing: DirectoryListing): void;
  onError(directory: string, error: unknown): void;
}

interface LeaseRecord {
  release?: () => void;
  /** Set when the directory stopped being wanted before its watch arrived. */
  retired: boolean;
  /**
   * Stops the bootstrap listing when the directory stops being wanted.
   *
   * The bootstrap is the directory's listing, so without this a folder opened
   * and immediately closed still transferred its whole contents before the
   * lease was thrown away.
   */
  abort: AbortController;
}

/**
 * The set of directory watches this Explorer holds, kept as a keyed map so a
 * change to which directories are open costs only the difference.
 *
 * Rebuilding the set instead — release everything, re-acquire everything —
 * turned opening one folder into an unwatch and a watch per already-open
 * folder, which on the remote link is a burst of round trips and a burst of
 * host watcher registrations for a tree that barely moved.
 */
export class DirectoryWatchLeases {
  readonly #leases = new Map<string, LeaseRecord>();
  /**
   * Directories the host refused, and when they may be asked again.
   *
   * Without this, a refusal — the host's 128-watch limit, an exhausted inotify
   * budget — was retried on every expand or collapse anywhere in the tree, and
   * each refusal cost a fallback directory list. Twenty refused directories
   * turned one keystroke into forty remote round trips.
   */
  readonly #refused = new Map<string, { until: number; wait: number }>();

  /** Acquires watches for directories that gained one and releases the rest. */
  sync(desired: readonly string[], host: WatchLeaseHost, now = Date.now()): void {
    const wanted = new Set(desired);
    for (const [directory, record] of [...this.#leases]) {
      if (!wanted.has(directory)) this.#retire(directory, record);
    }
    for (const directory of [...this.#refused.keys()]) {
      if (!wanted.has(directory)) this.#refused.delete(directory);
    }
    for (const directory of wanted) {
      if (this.#leases.has(directory)) continue;
      const refusal = this.#refused.get(directory);
      if (refusal && now < refusal.until) continue;
      const record: LeaseRecord = { retired: false, abort: new AbortController() };
      this.#leases.set(directory, record);
      void host.acquire(directory, record.abort.signal).then((lease) => {
        if (record.retired || this.#leases.get(directory) !== record) {
          lease.release();
          return;
        }
        record.release = lease.release;
        this.#refused.delete(directory);
        host.onBootstrap(directory, lease.snapshot);
      }).catch((error) => {
        if (this.#leases.get(directory) === record) this.#leases.delete(directory);
        if (record.retired) return;
        const wait = Math.min(
          (this.#refused.get(directory)?.wait ?? WATCH_RETRY_BASE_MS / 2) * 2,
          WATCH_RETRY_MAX_MS,
        );
        this.#refused.set(directory, { until: Date.now() + wait, wait });
        host.onError(directory, error);
      });
    }
  }

  releaseAll(): void {
    for (const [directory, record] of [...this.#leases]) this.#retire(directory, record);
    this.#refused.clear();
  }

  get held(): number {
    return this.#leases.size;
  }

  #retire(directory: string, record: LeaseRecord): void {
    this.#leases.delete(directory);
    record.retired = true;
    // An arrived watch is released; one still in flight is stopped where it is.
    if (record.release) record.release();
    else record.abort.abort();
  }
}
