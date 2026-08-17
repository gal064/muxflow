/**
 * What a directory read is for.
 *
 * A `"page"` read *extends* a listing rather than replacing one, so it is not a
 * rival of anything and nothing supersedes it: keying only on the path made
 * every "Load more" click silently abandon any recovery in flight for that
 * directory, and nothing rescheduled it. `"list"` and `"restore"` both answer
 * the question "what does this directory contain now", so they do supersede
 * each other and the later one wins — a Refresh raised over a running restore
 * is the newer question, and so is the reverse.
 *
 * Stated this way round because the previous wording claimed the broader rule —
 * that reads for *different* purposes are never rivals — which is true of
 * `"page"` and of nothing else, and a cancellation ledger guarded by an
 * invariant it does not hold is worse than one with no comment at all.
 */
export type RequestKind = "list" | "page" | "restore";

interface InFlight {
  path: string;
  controller: AbortController;
}

/** One directory read's claim on the tree, handed back when it settles. */
export interface RequestSlot {
  /** The signal the read must carry, so abandoning it stops the host too. */
  signal: AbortSignal;
  /** Whether this read is still the one whose answer the tree wants. */
  current(): boolean;
  /** Releases the slot. Safe to call more than once. */
  close(): void;
}

/**
 * Every directory read this Explorer has in flight, and which of them is still
 * wanted.
 *
 * One owner for three facts that were previously three maps mutated together —
 * the per-path supersession serial, the abort controllers, and the teardown —
 * so a read cannot be cancelled without being superseded, or superseded
 * without being cancelled. Every place that gave up on a read had to remember
 * all three; the one that forgot let a late answer land in a directory the
 * tree could no longer reach.
 */
export class DirectoryRequests {
  readonly #inflight = new Map<string, InFlight>();
  readonly #serial = new Map<string, number>();

  /**
   * Claims the slot for one read.
   *
   * A page read deliberately does not supersede the directory: it *extends* a
   * listing rather than replacing one, and the page it carries is checked
   * against the held listing's own revision when it lands.
   */
  open(path: string, kind: RequestKind): RequestSlot {
    const key = `${path}\0${kind}`;
    this.#inflight.get(key)?.controller.abort();
    const controller = new AbortController();
    this.#inflight.set(key, { path, controller });
    if (kind !== "page") this.#bump(path);
    const serial = this.#serial.get(path) ?? 0;
    return {
      signal: controller.signal,
      current: () => !controller.signal.aborted
        && (kind === "page" || (this.#serial.get(path) ?? 0) === serial),
      close: () => {
        if (this.#inflight.get(key)?.controller === controller) this.#inflight.delete(key);
      },
    };
  }

  /**
   * Whether a read of this exact kind is already running for this directory.
   *
   * The one owner of this fact answering it, rather than a caller inferring it
   * from whatever the tree has already installed. A read that has not landed
   * yet has installed nothing, so "no listing" and "no read" look identical
   * from the outside — and a caller that asks again on that basis does not
   * merely duplicate the read, it *aborts* the one that was already fetching.
   */
  reading(path: string, kind: RequestKind): boolean {
    return this.#inflight.has(`${path}\0${kind}`);
  }

  /**
   * Declares every read of one directory out of date, and stops them.
   *
   * Used when a listing is installed outright: whatever else is in flight for
   * that directory describes it as it was.
   */
  supersede(path: string): void {
    this.#bump(path);
    this.abort((candidate) => candidate === path);
  }

  /** Stops every read whose directory matches, and refuses their answers. */
  abort(matches: (path: string) => boolean): void {
    for (const [key, held] of [...this.#inflight]) {
      if (!matches(held.path)) continue;
      this.#inflight.delete(key);
      this.#bump(held.path);
      held.controller.abort();
    }
  }

  clear(): void {
    this.abort(() => true);
    this.#serial.clear();
  }

  get inFlight(): number {
    return this.#inflight.size;
  }

  #bump(path: string): void {
    this.#serial.set(path, (this.#serial.get(path) ?? 0) + 1);
  }
}
