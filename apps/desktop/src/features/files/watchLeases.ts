import type { DirectoryListing, DirectoryWatchLease } from "./types";

export interface WatchLeaseHost {
  acquire(directory: string): Promise<DirectoryWatchLease>;
  /** The bootstrap listing the host returned when the watch was armed. */
  onBootstrap(directory: string, listing: DirectoryListing): void;
  onError(directory: string, error: unknown): void;
}

interface LeaseRecord {
  release?: () => void;
  /** Set when the directory stopped being wanted before its watch arrived. */
  retired: boolean;
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

  /** Acquires watches for directories that gained one and releases the rest. */
  sync(desired: readonly string[], host: WatchLeaseHost): void {
    const wanted = new Set(desired);
    for (const [directory, record] of [...this.#leases]) {
      if (!wanted.has(directory)) this.#retire(directory, record);
    }
    for (const directory of wanted) {
      if (this.#leases.has(directory)) continue;
      const record: LeaseRecord = { retired: false };
      this.#leases.set(directory, record);
      void host.acquire(directory).then((lease) => {
        if (record.retired || this.#leases.get(directory) !== record) {
          lease.release();
          return;
        }
        record.release = lease.release;
        host.onBootstrap(directory, lease.snapshot);
      }).catch((error) => {
        if (this.#leases.get(directory) === record) this.#leases.delete(directory);
        if (!record.retired) host.onError(directory, error);
      });
    }
  }

  releaseAll(): void {
    for (const [directory, record] of [...this.#leases]) this.#retire(directory, record);
  }

  get held(): number {
    return this.#leases.size;
  }

  #retire(directory: string, record: LeaseRecord): void {
    this.#leases.delete(directory);
    record.retired = true;
    record.release?.();
  }
}
