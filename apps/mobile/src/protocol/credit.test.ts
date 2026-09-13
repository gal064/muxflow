import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { OutputCreditLedger, type OutputAck } from "./credit";

const WINDOW = BigInt(2 * 1024 * 1024);

describe("terminal output credit (§7.7)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("coalesces small charges into one cumulative ack after 50 ms", () => {
    const acks: OutputAck[] = [];
    const ledger = new OutputCreditLedger({ windowBytes: WINDOW, send: (ack) => acks.push(ack) });
    ledger.charge(100n, 1n);
    ledger.charge(200n, 1n);
    expect(acks).toEqual([]);
    vi.advanceTimersByTime(49);
    expect(acks).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(acks).toEqual([{ cumulativeBytes: 300n, cumulativeRecords: 2n }]);
    // Nothing new: no empty ack.
    vi.advanceTimersByTime(100);
    expect(acks).toHaveLength(1);
  });

  it("acks immediately once un-acked bytes exceed 25 % of the window", () => {
    const acks: OutputAck[] = [];
    const ledger = new OutputCreditLedger({ windowBytes: WINDOW, send: (ack) => acks.push(ack) });
    const quarter = WINDOW / 4n;
    ledger.charge(quarter, 1n);
    expect(acks).toEqual([]);
    ledger.charge(1n, 1n);
    expect(acks).toEqual([{ cumulativeBytes: quarter + 1n, cumulativeRecords: 2n }]);
    // The timer armed by the first charge does not send a duplicate.
    vi.advanceTimersByTime(50);
    expect(acks).toHaveLength(1);
  });

  it("never acks more than it was charged, and totals are cumulative across acks", () => {
    const acks: OutputAck[] = [];
    const ledger = new OutputCreditLedger({ windowBytes: WINDOW, send: (ack) => acks.push(ack) });
    ledger.charge(10n, 1n);
    vi.advanceTimersByTime(50);
    ledger.charge(5n, 1n);
    vi.advanceTimersByTime(50);
    expect(acks).toEqual([
      { cumulativeBytes: 10n, cumulativeRecords: 1n },
      { cumulativeBytes: 15n, cumulativeRecords: 2n },
    ]);
    expect(ledger.acknowledged).toEqual(ledger.charged);
  });

  it("rejects a missing credit window and sends nothing after close", () => {
    const acks: OutputAck[] = [];
    expect(() => new OutputCreditLedger({ windowBytes: 0n, send: (ack) => acks.push(ack) }))
      .toThrow("positive window");
    const ledger = new OutputCreditLedger({ windowBytes: WINDOW, send: (ack) => acks.push(ack) });
    ledger.charge(1n, 1n);
    ledger.close();
    vi.advanceTimersByTime(100);
    expect(acks).toEqual([]);
  });
});
