// The app has one native recorder but may keep several voice controllers alive.
// Keep native transitions ordered here, at the same process-global boundary as
// the controller registry, and expose only the cross-session listening fact
// needed to suppress reply autoplay.

export class VoiceRecorderCoordinator {
  private queue: Promise<void> = Promise.resolve();
  private readonly listeners = new Set<symbol>();
  private readonly waitingClaims: Array<() => void> = [];
  private owner: symbol | undefined;

  /** Prepare (or re-prepare) the shared recorder for this controller. */
  claim(owner: symbol, isCurrent: () => boolean, operation: () => void | PromiseLike<void>): Promise<boolean> {
    return new Promise<boolean>((resolve, reject) => {
      const attempt = (): void => {
        void this.enqueue(async () => {
          if (!isCurrent()) {
            resolve(false);
            if (this.owner === undefined) this.wakeNextClaim();
            return;
          }
          if (this.owner !== undefined && this.owner !== owner) {
            this.waitingClaims.push(attempt);
            return;
          }
          try {
            await operation();
            this.owner = owner;
            resolve(true);
          } catch (error) {
            reject(error);
            if (this.owner === undefined) this.wakeNextClaim();
          }
        });
      };
      attempt();
    });
  }

  /** Run a recorder operation only while this controller still owns it. */
  runOwned<T>(owner: symbol, operation: () => T | PromiseLike<T>): Promise<T | undefined> {
    return this.enqueue(() => this.owner === owner ? operation() : undefined);
  }

  /** Finish an owner's use as one queue entry, then give up ownership even when it rejects. */
  teardown<T>(owner: symbol, operation: () => T | PromiseLike<T>): Promise<T | undefined> {
    return this.enqueue(async () => {
      if (this.owner !== owner) return undefined;
      try {
        return await operation();
      } finally {
        if (this.owner === owner) {
          this.owner = undefined;
          this.wakeNextClaim();
        }
      }
    });
  }

  /** Release an idle recorder only if nobody else has claimed it meanwhile. */
  release(owner: symbol, shouldRelease: () => boolean, operation: () => void): Promise<void> {
    return this.enqueue(() => {
      if (this.owner !== owner || !shouldRelease()) return;
      try {
        operation();
      } finally {
        this.owner = undefined;
        this.wakeNextClaim();
      }
    });
  }

  private wakeNextClaim(): void {
    this.waitingClaims.shift()?.();
  }

  private enqueue<T>(operation: () => T | PromiseLike<T>): Promise<T> {
    const result = this.queue.then(operation);
    this.queue = result.then(() => undefined, () => undefined);
    return result;
  }

  setListening(owner: symbol, listening: boolean): void {
    if (listening) this.listeners.add(owner);
    else this.listeners.delete(owner);
  }

  isListeningFor(owner: symbol): boolean {
    return this.listeners.has(owner);
  }

  get isListening(): boolean {
    return this.listeners.size > 0;
  }
}
