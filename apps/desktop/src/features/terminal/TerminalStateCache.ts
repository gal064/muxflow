const encoder = new TextEncoder();

export interface CachedTerminalState {
  serialized: string;
  savedAt: number;
  byteLength: number;
  terminalEpoch?: number;
  outputGeneration: number;
  /**
   * Whether this screen is a photograph of the visible grid with nothing above
   * it — a host seed that no history has been spliced onto yet.
   *
   * It has to be kept here because it is a property of *these bytes*, not of
   * the terminal that produced them. A pane seeded screen-only, hidden, and
   * then restored from this entry is showing scrollback it does not have; its
   * next mount is a fresh closure, and without this it never asks for the
   * history it is missing.
   */
  screenSeeded: boolean;
  /**
   * Whether tmux has already answered "there is nothing further above this" for
   * the buffer these bytes came from.
   *
   * Kept for the same reason as `screenSeeded`, and read only when it is true:
   * a pane that walked to the top of tmux's history and is then restored here
   * must not walk it again, and a request from the top answers with one clamped
   * row rather than with nothing, so nothing else would stop it.
   */
  historyExhausted: boolean;
  /**
   * How many pages of scrollback are already spliced into these bytes.
   *
   * Not load-bearing for correctness — the next request starts above whatever
   * rows the restored buffer reports holding, whatever this says — but it is
   * how a restored pane's paging shows up in the journal, and how a test tells
   * "restored partway up its history" from "restored at the bottom of it".
   */
  historyPagesLoaded: number;
  /**
   * How many lines the *next* page above these bytes should ask for.
   *
   * Paging grows geometrically, because each page is applied by rewriting the
   * whole buffer: N pages of a fixed size cost O(N^2) bytes through xterm and,
   * past a couple of hundred kilobytes, split the reset and the content across
   * frames — which the user reads as a flicker. Kept here for the same reason
   * as `historyPagesLoaded`: the growth is a property of how far these bytes
   * have already been paged, and a restore that forgot it would start the
   * ladder again from the smallest page.
   *
   * Zero means "these bytes say nothing about it", which the pane reads as the
   * first page size — an entry written before this field existed, and a screen
   * nobody has paged, are the same case.
   */
  historyNextPageLines: number;
}

/** What a screen carries about how far up its own history it has been fetched. */
export interface CachedHistoryState {
  screenSeeded: boolean;
  historyExhausted: boolean;
  historyPagesLoaded: number;
  historyNextPageLines: number;
}

export class TerminalStateCache {
  readonly #states = new Map<string, CachedTerminalState>();
  #retainedBytes = 0;

  /**
   * @param capacity how many panes keep their screen here at once.
   *
   * One entry per pane the user has open, not per pane on screen: this is what
   * a hidden pane's reveal resumes from, and an eviction costs that pane a full
   * host seed on its next reveal — the payload the whole switch-cost change
   * exists to avoid. Twenty was below the number of panes a real session has
   * (21 in the drill that measured this), so ordinary switching evicted panes
   * that were about to be revealed again. Sixty-four is above it with room, and
   * costs nothing when unused: the entries are only as large as the screens
   * actually stored, and `maxTotalBytes` — not the count — is what bounds the
   * memory.
   */
  constructor(
    readonly capacity = 64,
    readonly maxSerializedBytes = 2_000_000,
    // Sixty-four screens at the ~40 KB a serialized 200x50 grid weighs is under
    // 3 MB; this is the bound for the pathological case, where a few panes hold
    // very large buffers. It is enforced on every insert, so the count and the
    // bytes are both hard limits and the larger capacity cannot turn into a
    // larger footprint.
    //
    // It counts UTF-8 bytes and retains a UTF-16 string, so the resident cost
    // of a screen is about twice what is charged for it: mostly-ASCII terminal
    // output is one byte here and two in memory. The bound is deliberately the
    // wire-shaped number — it is the same measurement the host-side budget for
    // a screen uses — so read this as ~48 MB of heap at the ceiling, not 24.
    readonly maxTotalBytes = 24 * 1024 * 1024,
  ) {}

  /**
   * One pane's screen, and a use that counts as recent.
   *
   * The delete-then-set is the whole of the LRU: `Map` iterates in insertion
   * order and eviction takes the front of it, so a read that did not reorder
   * would make this a FIFO — and a FIFO evicts the pane the user keeps coming
   * back to. It is on the read rather than only on the write because a reveal
   * reads this entry and does not necessarily write one.
   */
  get(paneId: string): CachedTerminalState | undefined {
    const value = this.#states.get(paneId);
    if (!value) return undefined;
    this.#states.delete(paneId);
    this.#states.set(paneId, value);
    return value;
  }

  /**
   * Keeps one pane's screen, measuring it here.
   *
   * The measurement used to be done by the encoder that also produced the bytes
   * for the host. There are no such bytes any more — this cache *is* where a
   * hidden pane's screen lives — so the only thing left to weigh is the string,
   * and a screen too large for this cache is simply not kept: the pane's next
   * reveal is answered with a seed, which is the same recovery without a
   * sentence about it.
   */
  set(
    paneId: string,
    serialized: string,
    checkpoint?: { terminalEpoch: number; outputGeneration: number },
    history?: CachedHistoryState,
  ): void {
    const byteLength = serialized ? encoder.encode(serialized).byteLength : 0;
    if (!serialized || byteLength > this.maxSerializedBytes || byteLength > this.maxTotalBytes) {
      this.delete(paneId);
      return;
    }
    this.delete(paneId);
    this.#states.set(paneId, {
      serialized,
      savedAt: Date.now(),
      byteLength,
      terminalEpoch: checkpoint?.terminalEpoch,
      outputGeneration: checkpoint?.outputGeneration ?? 0,
      screenSeeded: history?.screenSeeded ?? false,
      historyExhausted: history?.historyExhausted ?? false,
      historyPagesLoaded: history?.historyPagesLoaded ?? 0,
      historyNextPageLines: history?.historyNextPageLines ?? 0,
    });
    this.#retainedBytes += byteLength;
    while (this.#states.size > this.capacity || this.#retainedBytes > this.maxTotalBytes) {
      const oldest = this.#states.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.delete(oldest);
    }
  }

  delete(paneId: string): void {
    const value = this.#states.get(paneId);
    if (value) this.#retainedBytes -= value.byteLength;
    this.#states.delete(paneId);
  }

  clear(): void {
    this.#states.clear();
    this.#retainedBytes = 0;
  }

  get size(): number {
    return this.#states.size;
  }

  get retainedByteLength(): number {
    return this.#retainedBytes;
  }
}

export const terminalStateCache = new TerminalStateCache();
