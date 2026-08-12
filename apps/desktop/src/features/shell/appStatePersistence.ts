import type { PersistedAppState } from "./types";

type SaveAppState = (state: PersistedAppState) => Promise<void>;

/** Serializes saves, coalesces ordinary UI churn, and provides a close-time barrier. */
export class AppStatePersistence {
  private desired: { revision: number; state: PersistedAppState } | undefined;
  private revision = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private draining: Promise<void> | undefined;
  private lastError: unknown;

  constructor(
    private readonly save: SaveAppState,
    private readonly delayMs = 120,
    private readonly onError: (error: unknown) => void = () => undefined,
  ) {}

  schedule(state: PersistedAppState): void {
    this.desired = { revision: ++this.revision, state };
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.drain();
    }, this.delayMs);
  }

  async flush(state?: PersistedAppState): Promise<void> {
    if (state) this.desired = { revision: ++this.revision, state };
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    await this.drain();
    if (this.lastError !== undefined) throw this.lastError;
  }

  dispose(): void {
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
  }

  private async drain(): Promise<void> {
    if (this.draining) return this.draining;
    this.draining = (async () => {
      while (this.desired) {
        const candidate = this.desired;
        try {
          await this.save(candidate.state);
          this.lastError = undefined;
          if (this.desired?.revision === candidate.revision) this.desired = undefined;
        } catch (error) {
          this.lastError = error;
          this.onError(error);
          return;
        }
      }
    })();
    try {
      await this.draining;
    } finally {
      this.draining = undefined;
    }
  }
}
