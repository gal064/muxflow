export interface CachedTerminalState {
  serialized: string;
  savedAt: number;
  byteLength: number;
  terminalEpoch?: number;
  outputGeneration: number;
}

export class TerminalStateCache {
  readonly #states = new Map<string, CachedTerminalState>();
  readonly #encoder = new TextEncoder();
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

  set(
    paneId: string,
    serialized: string,
    checkpoint?: { terminalEpoch: number; outputGeneration: number },
    encodedByteLength?: number,
  ): void {
    const byteLength = encodedByteLength ?? this.#encoder.encode(serialized).byteLength;
    if (!Number.isSafeInteger(byteLength) || byteLength < 0) {
      this.delete(paneId);
      return;
    }
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
