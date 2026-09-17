/**
 * Tracks renderer generations without letting an old async xterm completion
 * cross an authoritative seed boundary.
 */
export class TerminalGenerationWatermark {
  #version = 0;
  #appliedGeneration = 0;
  #enqueuedGeneration = 0;

  get appliedGeneration(): number {
    return this.#appliedGeneration;
  }

  get enqueuedGeneration(): number {
    return this.#enqueuedGeneration;
  }

  resetAuthoritativeStream(): void {
    this.#version += 1;
    this.#appliedGeneration = 0;
    this.#enqueuedGeneration = 0;
  }

  enqueued(generation: number, onRendered?: () => void): () => void {
    if (Number.isSafeInteger(generation) && generation > this.#enqueuedGeneration) {
      this.#enqueuedGeneration = generation;
    }
    const version = this.#version;
    return () => {
      // The external completion still runs: its bytes did reach xterm. Only
      // its obsolete generation is forbidden from entering the new stream.
      if (version === this.#version
        && Number.isSafeInteger(generation)
        && generation > this.#appliedGeneration) {
        this.#appliedGeneration = generation;
      }
      onRendered?.();
    };
  }
}
