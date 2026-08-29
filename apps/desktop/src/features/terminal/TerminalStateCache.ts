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
}

export class TerminalStateCache {
  readonly #states = new Map<string, CachedTerminalState>();
  #retainedBytes = 0;

  constructor(
    readonly capacity = 20,
    readonly maxSerializedBytes = 2_000_000,
    readonly maxTotalBytes = 16 * 1024 * 1024,
  ) {}

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
    screenSeeded = false,
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
      screenSeeded,
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
