import type { DirectoryListing, DirectoryWatchLease } from "./types";

/** First wait after a refused watch, doubled on each further refusal. */
export const WATCH_RETRY_BASE_MS = 1_000;
export const WATCH_RETRY_MAX_MS = 30_000;

export interface WatchLeaseHost {
  acquire(directory: string, signal: AbortSignal): Promise<DirectoryWatchLease>;
  /**
   * The listing the host returned when the watch was armed.
   *
   * `fresh` is false when this Explorer joined a watch some other surface had
   * already established: the snapshot is then of unknown age and is a paint
   * rather than an authority. See [`DirectoryWatchLease`].
   */
  onBootstrap(directory: string, listing: DirectoryListing, fresh: boolean): void;
  onError(directory: string, error: unknown): void;
  /**
   * Called for a wanted directory this pass will not watch, because the host
   * refused it recently enough that its backoff has not expired.
   *
   * Without it the directory silently gets nothing at all — no watch, no
   * listing, and no error — so a tree that lost its watch budget showed folders
   * that stayed empty and marked busy forever.
   */
  onDeferred(directory: string): void;
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
  /** The last set asked for, so a backoff can expire without user activity. */
  #desired: readonly string[] = [];
  #host: WatchLeaseHost | undefined;
  #retry: ReturnType<typeof setTimeout> | undefined;

  /** Acquires watches for directories that gained one and releases the rest. */
  sync(desired: readonly string[], host: WatchLeaseHost, now = Date.now()): void {
    this.#desired = desired;
    this.#host = host;
    const wanted = new Set(desired);
    for (const [directory, record] of [...this.#leases]) {
      if (!wanted.has(directory)) this.#retire(directory, record);
    }
    for (const directory of [...this.#refused.keys()]) {
      if (!wanted.has(directory)) this.#refused.delete(directory);
    }
    let soonest: number | undefined;
    for (const directory of wanted) {
      if (this.#leases.has(directory)) continue;
      const refusal = this.#refused.get(directory);
      if (refusal && now < refusal.until) {
        soonest = soonest === undefined ? refusal.until : Math.min(soonest, refusal.until);
        host.onDeferred(directory);
        continue;
      }
      const record: LeaseRecord = { retired: false, abort: new AbortController() };
      this.#leases.set(directory, record);
      void host.acquire(directory, record.abort.signal).then((lease) => {
        if (record.retired || this.#leases.get(directory) !== record) {
          lease.release();
          return;
        }
        record.release = lease.release;
        this.#refused.delete(directory);
        host.onBootstrap(directory, lease.snapshot, lease.fresh);
      }).catch((error) => {
        if (this.#leases.get(directory) === record) this.#leases.delete(directory);
        if (record.retired) return;
        const wait = Math.min(
          (this.#refused.get(directory)?.wait ?? WATCH_RETRY_BASE_MS / 2) * 2,
          WATCH_RETRY_MAX_MS,
        );
        this.#refused.set(directory, { until: now + wait, wait });
        this.#schedule(wait);
        host.onError(directory, error);
      });
    }
    if (soonest !== undefined) this.#schedule(Math.max(0, soonest - now));
  }

  releaseAll(): void {
    for (const [directory, record] of [...this.#leases]) this.#retire(directory, record);
    this.#refused.clear();
    this.#desired = [];
    this.#host = undefined;
    if (this.#retry !== undefined) clearTimeout(this.#retry);
    this.#retry = undefined;
  }

  get held(): number {
    return this.#leases.size;
  }

  /**
   * Re-attempts a refused directory when its backoff expires.
   *
   * `sync` alone runs only when the set of open directories changes, so without
   * a timer a refused watch was never retried at all: the directory kept the
   * one fallback listing it was given and then stopped receiving changes for as
   * long as the user left the tree alone — which for a host that is briefly out
   * of inotify budget is indistinguishable from the Explorer being broken.
   */
  #schedule(delay: number): void {
    if (this.#retry !== undefined) return;
    this.#retry = setTimeout(() => {
      this.#retry = undefined;
      const host = this.#host;
      if (host && this.#desired.length > 0) this.sync(this.#desired, host);
    }, Math.max(1, delay));
  }

  #retire(directory: string, record: LeaseRecord): void {
    this.#leases.delete(directory);
    record.retired = true;
    // An arrived watch is released; one still in flight is stopped where it is.
    if (record.release) record.release();
    else record.abort.abort();
  }
}
