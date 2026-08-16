import type { OwnedTerminalBytes } from "./TerminalBytes";

export const MAX_DEFERRED_OUTPUT_BYTES = 1024 * 1024;
export const MAX_DEFERRED_OUTPUT_RECORDS = 1024;

export interface DeferredTerminalOutput {
  data: OwnedTerminalBytes;
  generation: number;
  terminalEpoch?: number;
}

export type DeferredOutputAdmission = "accepted" | "overflow" | "blocked";

/**
 * Owns output that arrived while pane recovery was deciding its authoritative
 * cutoff. Bytes alone are not a bound because an empty terminal record still
 * retains a generation, callback, and array slot.
 */
export class DeferredTerminalOutputQueue {
  #items: DeferredTerminalOutput[] = [];
  #bytes = 0;
  #blocked = false;

  enqueue(item: DeferredTerminalOutput): DeferredOutputAdmission {
    if (this.#blocked) return "blocked";
    if (this.#items.length >= MAX_DEFERRED_OUTPUT_RECORDS
      || this.#bytes + item.data.byteLength > MAX_DEFERRED_OUTPUT_BYTES) {
      this.#items = [];
      this.#bytes = 0;
      this.#blocked = true;
      return "overflow";
    }
    this.#items.push(item);
    this.#bytes += item.data.byteLength;
    return "accepted";
  }

  drain(): DeferredTerminalOutput[] {
    const items = this.#items;
    this.reset();
    return items;
  }

  reset(): void {
    this.#items = [];
    this.#bytes = 0;
    this.#blocked = false;
  }

  get recordCount(): number {
    return this.#items.length;
  }

  get byteLength(): number {
    return this.#bytes;
  }
}
