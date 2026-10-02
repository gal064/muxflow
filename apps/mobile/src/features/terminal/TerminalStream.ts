import type { GridFence } from "./bridgeMessages";
import type { Grid } from "./sizing";

interface StreamTerminal {
  cols: number;
  rows: number;
  write(bytes: Uint8Array, done: () => void): void;
  reset(): void;
  resize(cols: number, rows: number): void;
  scrollToLine(line: number): void;
}

/** Serialize mutations through xterm's parser, including reset and resize. */
export class TerminalStream {
  private pending = Promise.resolve();
  private batch: { chunks: Uint8Array[]; bytes: number } | undefined;

  constructor(
    private readonly terminal: StreamTerminal,
    private readonly written: (bytes: number) => void,
    private readonly failed: (error: unknown) => void,
  ) {}

  drain(): Promise<void> { return this.pending; }

  resize(grid: Grid): void {
    this.batch = undefined;
    this.enqueue(async () => this.setGrid(grid));
  }

  write(bytes: Uint8Array, reset = false, grid?: Grid): void {
    // Preserve xterm's batching while mutations remain ordered: a burst of
    // output behind a seed or resize should cost one parser turn, not one
    // timer turn per record.
    if (!reset && !grid) {
      if (this.batch && this.batch.bytes + bytes.byteLength <= 256 * 1024) {
        this.batch.chunks.push(bytes);
        this.batch.bytes += bytes.byteLength;
        return;
      }
      const batch = { chunks: [bytes], bytes: bytes.byteLength };
      this.batch = batch;
      this.enqueue(async () => {
        if (this.batch === batch) this.batch = undefined;
        let output = batch.chunks[0]!;
        if (batch.chunks.length > 1) {
          output = new Uint8Array(batch.bytes);
          let offset = 0;
          for (const chunk of batch.chunks) {
            output.set(chunk, offset);
            offset += chunk.byteLength;
          }
        }
        await this.parse(output);
        this.written(batch.bytes);
      });
      return;
    }
    this.batch = undefined;
    this.enqueue(async () => {
      if (reset) this.terminal.reset();
      if (grid) this.setGrid(grid);
      await this.parse(bytes);
      this.written(bytes.byteLength);
    });
  }

  splice(hist: Uint8Array, rowsAdded: number, tail: Uint8Array, grids: GridFence[]): void {
    this.batch = undefined;
    this.enqueue(async () => {
      this.terminal.reset();
      const first = grids[0];
      if (first?.offset === 0) this.setGrid(first);
      if (hist.byteLength > 0) await this.parse(hist);
      await this.parse(new TextEncoder().encode("\r\n".repeat(this.terminal.rows)));
      let offset = 0;
      for (const grid of grids) {
        if (grid.offset > offset) await this.parse(tail.subarray(offset, grid.offset));
        this.setGrid(grid);
        offset = grid.offset;
      }
      await this.parse(tail.subarray(offset));
      this.terminal.scrollToLine(rowsAdded);
      this.written(hist.byteLength + tail.byteLength);
    });
  }

  private setGrid(grid: Grid): void {
    if (this.terminal.cols !== grid.cols || this.terminal.rows !== grid.rows) {
      this.terminal.resize(grid.cols, grid.rows);
    }
  }

  private parse(bytes: Uint8Array): Promise<void> {
    return new Promise((resolve) => this.terminal.write(bytes, resolve));
  }

  private enqueue(operation: () => Promise<void>): void {
    this.pending = this.pending.then(operation).catch(this.failed);
  }
}
