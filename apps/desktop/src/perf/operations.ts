/**
 * A deliberately tiny deterministic measurement seam for Phase 14 fixtures.
 * Production callers omit it, so hot paths do not allocate or mutate counters.
 */
export interface OperationRecorder {
  add(name: string, delta?: number): void;
  highWater?(name: string, value: number): void;
}

export class OperationCounters implements OperationRecorder {
  readonly #values = new Map<string, number>();
  readonly #highWater = new Map<string, number>();

  add(name: string, delta = 1): void {
    this.#values.set(name, (this.#values.get(name) ?? 0) + delta);
  }

  highWater(name: string, value: number): void {
    this.#highWater.set(name, Math.max(value, this.#highWater.get(name) ?? 0));
  }

  snapshot(): { counters: Record<string, number>; highWater: Record<string, number> } {
    const sorted = (values: Map<string, number>) =>
      Object.fromEntries([...values].sort(([left], [right]) => left.localeCompare(right)));
    return { counters: sorted(this.#values), highWater: sorted(this.#highWater) };
  }
}
