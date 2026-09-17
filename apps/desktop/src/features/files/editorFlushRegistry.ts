type Flush = () => Promise<void>;

class EditorFlushRegistry {
  readonly #active = new Map<string, Flush>();
  readonly #pending = new Set<Promise<void>>();

  register(id: string, flush: Flush): () => void {
    this.#active.set(id, flush);
    return () => { if (this.#active.get(id) === flush) this.#active.delete(id); };
  }

  track(promise: Promise<void>): void {
    this.#pending.add(promise);
    void promise.finally(() => this.#pending.delete(promise)).catch(() => undefined);
  }

  async flushAll(): Promise<void> {
    const started = [...this.#active.values()].map((flush) => flush());
    for (const promise of started) this.track(promise);
    const results = await Promise.allSettled([...this.#pending]);
    const failures = results.filter((result): result is PromiseRejectedResult => result.status === "rejected");
    if (failures.length) throw new AggregateError(failures.map((failure) => failure.reason), "One or more editor files could not be saved.");
  }
}

export const editorFlushRegistry = new EditorFlushRegistry();
