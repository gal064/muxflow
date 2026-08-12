import type { TextFile, WriteTextResult } from "./types";

export type SaveState = "saved" | "dirty" | "saving" | "error";

export interface AutosaveSnapshot {
  content: string;
  generation: string;
  lineEnding: TextFile["lineEnding"];
}

export interface AutosaveView {
  content: string;
  generation: string;
  lineEnding: TextFile["lineEnding"];
  state: SaveState;
  error?: string;
}

/** Serializes debounced saves and admits newer external generations. */
export class AutosaveController {
  private timer?: ReturnType<typeof setTimeout>;
  private revision = 0;
  private externalEpoch = 0;
  private inFlight?: Promise<void>;
  private selfOperations = new Set<string>();
  private view: AutosaveView;

  constructor(
    initial: AutosaveSnapshot,
    private readonly save: (snapshot: AutosaveSnapshot, operationId: string) => Promise<WriteTextResult>,
    private readonly changed: (view: AutosaveView) => void,
    private readonly delayMillis = 150,
  ) {
    this.view = { ...initial, state: "saved" };
  }

  edit(content: string, lineEnding = this.lineEnding()): void {
    this.revision += 1;
    this.view = { ...this.view, content, lineEnding, state: "dirty", error: undefined };
    this.changed(this.view);
    if (this.timer) clearTimeout(this.timer);
    const revision = this.revision;
    this.timer = setTimeout(() => { void this.flushRevision(revision, lineEnding); }, this.delayMillis);
  }

  external(snapshot: AutosaveSnapshot, operationId?: string): void {
    if (operationId && this.selfOperations.has(operationId)) return;
    // A genuinely external write invalidates the suppression tokens for older
    // in-flight local writes. If one of those commits afterward, its host
    // change event is now a real last-writer transition and must be reloaded.
    this.selfOperations.clear();
    this.revision += 1;
    this.externalEpoch += 1;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    this.view = { ...snapshot, state: "saved" };
    this.changed(this.view);
  }

  current(): AutosaveView { return this.view; }

  async flush(): Promise<void> {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    // Never enqueue a duplicate of a save already in flight. First observe its
    // result; only a newer dirty/error revision still needs a commit.
    await this.inFlight;
    if (this.view.state === "dirty" || this.view.state === "error") {
      await this.flushRevision(this.revision, this.lineEnding());
      await this.inFlight;
    }
    if (this.view.state === "error") throw new Error(this.view.error ?? "The file could not be saved.");
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
  }

  private lineEnding(): TextFile["lineEnding"] {
    return this.view.lineEnding;
  }

  private async flushRevision(revision: number, lineEnding: TextFile["lineEnding"]): Promise<void> {
    if (revision !== this.revision || this.view.state === "saved") return;
    const content = this.view.content;
    const externalEpoch = this.externalEpoch;
    const operationId = crypto.randomUUID();
    this.selfOperations.add(operationId);
    if (this.selfOperations.size > 32) this.selfOperations.delete(this.selfOperations.values().next().value!);
    this.view = { ...this.view, state: "saving", error: undefined };
    this.changed(this.view);
    const run = async () => {
      if (revision !== this.revision) return;
      try {
        const result = await this.save({ content, generation: this.view.generation, lineEnding }, operationId);
        if (externalEpoch !== this.externalEpoch) return;
        if (revision === this.revision) this.view = { content, generation: result.generation, lineEnding, state: "saved" };
        else this.view = { ...this.view, generation: result.generation, state: "dirty" };
      } catch (error) {
        if (revision !== this.revision || externalEpoch !== this.externalEpoch) return;
        this.view = { ...this.view, state: "error", error: String(error) };
      }
      this.changed(this.view);
    };
    this.inFlight = (this.inFlight ?? Promise.resolve()).then(run);
    await this.inFlight;
  }
}
