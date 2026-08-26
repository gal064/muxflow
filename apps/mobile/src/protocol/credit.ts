// Terminal output credit accounting (design doc §7.7).
//
// The host stops sending terminal output once `terminalOutputWindowBytes` /
// `terminalOutputWindowRecords` of unacknowledged payload are outstanding
// (apps/host/src/service/terminal/output_credit.rs). Every TERMINAL_SEED /
// TERMINAL_OUTPUT event carries its charge in `terminalDeliveryBytes` /
// `terminalDeliveryRecords`; the client acknowledges cumulative totals. An ack
// above what was admitted, or below a previous ack, is a protocol violation
// that closes the connection, so this ledger only ever reports exactly what
// `charge` was told.

export interface OutputAck {
  cumulativeBytes: bigint;
  cumulativeRecords: bigint;
}

export interface OutputCreditLedgerOptions {
  /** `serverHello.terminalOutputWindowBytes`; 0 means credit was not negotiated and nothing is acked. */
  windowBytes: bigint;
  send: (ack: OutputAck) => void;
  /** Coalescing delay; the doc fixes it at 50 ms. */
  flushDelayMs?: number;
  setTimeout?: (callback: () => void, ms: number) => unknown;
  clearTimeout?: (handle: unknown) => void;
}

export class OutputCreditLedger {
  private cumulativeBytes = 0n;
  private cumulativeRecords = 0n;
  private ackedBytes = 0n;
  private ackedRecords = 0n;
  private timer: unknown;
  private closed = false;
  private readonly windowBytes: bigint;
  private readonly immediateThreshold: bigint;
  private readonly flushDelayMs: number;
  private readonly send: (ack: OutputAck) => void;
  private readonly scheduleTimeout: (callback: () => void, ms: number) => unknown;
  private readonly cancelTimeout: (handle: unknown) => void;

  constructor(options: OutputCreditLedgerOptions) {
    this.windowBytes = options.windowBytes;
    this.immediateThreshold = options.windowBytes / 4n;
    this.flushDelayMs = options.flushDelayMs ?? 50;
    this.send = options.send;
    this.scheduleTimeout = options.setTimeout ?? ((callback, ms) => setTimeout(callback, ms));
    this.cancelTimeout = options.clearTimeout ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
  }

  get enabled(): boolean {
    return this.windowBytes > 0n;
  }

  get charged(): OutputAck {
    return { cumulativeBytes: this.cumulativeBytes, cumulativeRecords: this.cumulativeRecords };
  }

  get acknowledged(): OutputAck {
    return { cumulativeBytes: this.ackedBytes, cumulativeRecords: this.ackedRecords };
  }

  /**
   * Records one delivered (or discarded — still charged) terminal event and
   * sends the ack now if the un-acked total exceeds 25 % of the window, else
   * within `flushDelayMs`.
   */
  charge(bytes: bigint, records: bigint): void {
    if (!this.enabled || this.closed) return;
    this.cumulativeBytes += bytes;
    this.cumulativeRecords += records;
    if (this.cumulativeBytes - this.ackedBytes > this.immediateThreshold) {
      this.flush();
      return;
    }
    if (this.timer === undefined) {
      this.timer = this.scheduleTimeout(() => {
        this.timer = undefined;
        this.flush();
      }, this.flushDelayMs);
    }
  }

  /** Sends the current cumulative totals if anything is un-acked. */
  flush(): void {
    if (this.timer !== undefined) {
      this.cancelTimeout(this.timer);
      this.timer = undefined;
    }
    if (!this.enabled || this.closed) return;
    if (this.cumulativeBytes === this.ackedBytes && this.cumulativeRecords === this.ackedRecords) return;
    this.ackedBytes = this.cumulativeBytes;
    this.ackedRecords = this.cumulativeRecords;
    this.send({ cumulativeBytes: this.ackedBytes, cumulativeRecords: this.ackedRecords });
  }

  /** Stops the timer; nothing is sent after this (the connection is gone). */
  close(): void {
    this.closed = true;
    if (this.timer !== undefined) {
      this.cancelTimeout(this.timer);
      this.timer = undefined;
    }
  }
}
