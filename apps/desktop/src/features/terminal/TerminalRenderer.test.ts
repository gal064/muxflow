import { describe, expect, it, vi } from "vitest";
import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { SearchAddon } from "@xterm/addon-search";
import { SerializeAddon } from "@xterm/addon-serialize";
import { restoreDecision, type GridOutcome, type TerminalSize } from "./TerminalRenderer";
import { TerminalWriteScheduler } from "./TerminalWriteScheduler";
import { interceptTerminalPlainTextPaste, isForcedLocalSelection, paneRecoveryPlan, reconcilePaneGrid } from "./TerminalPane";
import type { Pane } from "../../app/types";
import { OperationCounters } from "../../perf/operations";
import { TerminalEventHub } from "./TerminalEventHub";
import { decodeTerminalEvent } from "./api";
import { copyTerminalBytes } from "./TerminalBytes";

function wireOutputFrame(sequence: number, generation: number, data: Uint8Array): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(21 + data.byteLength);
  bytes.set([2, 0, 2, 0x25, 0x31]);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(5, BigInt(sequence), false);
  view.setBigUint64(13, BigInt(generation), false);
  bytes.set(data, 21);
  return bytes;
}

describe("TerminalWriteScheduler", () => {
  it("preserves byte order and respects the per-frame budget", () => {
    const frames: FrameRequestCallback[] = [];
    const written: number[][] = [];
    const pending: number[] = [];
    const completions: Array<() => void> = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => { written.push(Array.from(chunk)); completions.push(done); },
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      3,
      10,
      (bytes) => pending.push(bytes),
    );
    scheduler.enqueue(Uint8Array.from([1, 2, 3, 4]));
    scheduler.enqueue(Uint8Array.from([5, 6]));
    // The first event took the idle fast path and was cut at the 3-byte budget.
    frames.shift()!(0);
    expect(written).toEqual([[1, 2, 3]]);
    expect(scheduler.pendingBytes).toBe(6);
    expect(frames).toHaveLength(0);
    completions.shift()!();
    expect(scheduler.pendingBytes).toBe(3);
    // One frame carries the rest of the split event and the event behind it,
    // coalesced into a single write, still in byte order.
    frames.shift()!(16);
    expect(written).toEqual([[1, 2, 3], [4, 5, 6]]);
    completions.shift()!();
    expect(scheduler.pendingBytes).toBe(0);
    expect(pending.at(-1)).toBe(0);
  });

  it("cancels and drops queued work on disposal", async () => {
    let cancelled = 0;
    const completions: Array<() => void> = [];
    const scheduler = new TerminalWriteScheduler(
      (_chunk, done) => completions.push(done),
      () => 7,
      () => { cancelled += 1; },
    );
    scheduler.enqueue(Uint8Array.of(1));
    scheduler.enqueue(Uint8Array.of(2, 3));
    // The first byte took the idle fast path and is inside xterm's parser,
    // where nothing will ever complete it once the terminal is disposed.
    const drained = scheduler.sealAndDrain();
    scheduler.dispose();
    scheduler.enqueue(Uint8Array.of(4));
    expect(cancelled).toBeGreaterThan(0);
    expect(scheduler.pendingBytes).toBe(0);
    // Disposal must release the drain, or the next reveal of this pane — which
    // waits on it — never happens.
    await expect(drained).resolves.toBeUndefined();
    // A completion arriving after disposal is harmless.
    completions.shift()!();
    expect(scheduler.pendingBytes).toBe(0);
  });

  it("writes immediately when idle and falls back to frame pacing under load", () => {
    const frames: FrameRequestCallback[] = [];
    const written: number[][] = [];
    const completions: Array<() => void> = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => { written.push(Array.from(chunk)); completions.push(done); },
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      64,
      1024,
    );
    // An echoed keystroke arriving into an empty queue must not wait a frame.
    scheduler.enqueue(Uint8Array.of(1));
    expect(written).toEqual([[1]]);
    completions.shift()!();

    // A second write in the same frame is paced, so a flood cannot spin.
    scheduler.enqueue(Uint8Array.of(2));
    expect(written).toEqual([[1]]);
    const paced = frames.pop()!;
    paced(0);
    expect(written).toEqual([[1], [2]]);
    completions.shift()!();

    // Once a frame boundary passes, the fast path is available again.
    frames.shift()!(16);
    scheduler.enqueue(Uint8Array.of(3));
    expect(written).toEqual([[1], [2], [3]]);
  });

  it("advances a rendered checkpoint only after every chunk reaches xterm", () => {
    const frames: FrameRequestCallback[] = [];
    const completions: Array<() => void> = [];
    const rendered: number[] = [];
    const scheduler = new TerminalWriteScheduler(
      (_chunk, done) => completions.push(done),
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      2,
      16,
    );
    scheduler.enqueue(Uint8Array.of(1, 2, 3), () => rendered.push(3));
    frames.shift()!(0);
    completions.shift()!();
    expect(rendered).toEqual([]);
    frames.shift()!(16);
    expect(rendered).toEqual([]);
    completions.shift()!();
    expect(rendered).toEqual([3]);
  });

  it("seals new writes and drains scheduled plus in-flight callbacks before resolving", async () => {
    const frames: FrameRequestCallback[] = [];
    const completions: Array<() => void> = [];
    const rendered: number[] = [];
    const scheduler = new TerminalWriteScheduler(
      (_chunk, done) => completions.push(done),
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      2,
      16,
    );
    scheduler.enqueue(Uint8Array.of(1, 2, 3), () => rendered.push(3));
    frames.shift()!(0);
    let drained = false;
    const drain = scheduler.sealAndDrain().then(() => { drained = true; });
    expect(scheduler.enqueue(Uint8Array.of(4))).toBe(false);
    completions.shift()!();
    await Promise.resolve();
    expect(drained).toBe(false);
    frames.shift()!(16);
    completions.shift()!();
    await drain;
    expect(rendered).toEqual([3]);
    expect(scheduler.pendingBytes).toBe(0);
  });

  it("bounds queued and in-flight writes and requests overflow recovery exactly once", () => {
    const frames: FrameRequestCallback[] = [];
    const overflow: number[] = [];
    const scheduler = new TerminalWriteScheduler(
      () => undefined,
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      4,
      5,
      undefined,
      (bytes) => overflow.push(bytes),
    );
    expect(scheduler.enqueue(Uint8Array.from([1, 2, 3, 4]))).toBe(true);
    expect(scheduler.enqueue(Uint8Array.from([5, 6]))).toBe(false);
    expect(scheduler.overflowed).toBe(true);
    // The first chunk went straight to xterm on the idle fast path and is still
    // in flight there; dropping the queue cannot retract bytes already handed
    // over, and pretending otherwise would under-report the real backlog.
    expect(scheduler.pendingBytes).toBe(4);
    expect(scheduler.enqueue(Uint8Array.of(7))).toBe(false);
    expect(overflow).toEqual([6]);
  });

  it("suppresses output after overflow, then applies one seed before every subsequent byte", () => {
    const frames: FrameRequestCallback[] = [];
    const written: number[][] = [];
    const completions: Array<() => void> = [];
    const overflow: number[] = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => { written.push(Array.from(chunk)); completions.push(done); },
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      20,
      10,
      undefined,
      (bytes) => overflow.push(bytes),
    );
    scheduler.enqueue(Uint8Array.from([1, 2, 3, 4]));
    frames.shift()!(0);
    expect(scheduler.enqueue(Uint8Array.from([5, 6, 7, 8, 9, 10, 11]))).toBe(false);
    expect(scheduler.enqueue(Uint8Array.of(12))).toBe(false);
    expect(overflow).toEqual([11]);

    scheduler.replace(Uint8Array.of(19), false);
    expect(scheduler.enqueue(Uint8Array.of(19))).toBe(false);
    scheduler.replace(Uint8Array.of(20));
    expect(scheduler.enqueue(Uint8Array.from([21, 22]))).toBe(true);
    completions.shift()!();
    frames.shift()!(16);
    completions.shift()!();
    expect(written.flat()).toEqual([1, 2, 3, 4, 0x1b, 0x63, 20, 21, 22]);
    expect(overflow).toEqual([11]);
    expect(scheduler.pendingBytes).toBe(0);
  });

  it("owns queued bytes against source mutation until xterm acknowledges them", () => {
    const completions: Array<() => void> = [];
    const written: Uint8Array[] = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => { written.push(chunk); completions.push(done); },
      () => 1,
      () => undefined,
    );
    const wire = wireOutputFrame(1, 1, Uint8Array.of(1, 2, 3));
    const event = decodeTerminalEvent(wire.buffer);
    expect(event.kind).toBe("output");
    if (event.kind !== "output") throw new Error("expected output fixture");
    // Decode is the one transport ownership copy; the scheduler transfers that
    // exclusive allocation without copying it again.
    expect(event.data.buffer).not.toBe(wire.buffer);
    expect(event.data.buffer.byteLength).toBe(event.data.byteLength);
    wire.fill(9, 21);
    scheduler.enqueueOwned(event.data);
    expect([...written[0]]).toEqual([1, 2, 3]);
    completions.shift()!();
    expect(scheduler.pendingBytes).toBe(0);
  });

  it("copies a borrowed scheduler input before its caller can mutate it", () => {
    const written: Uint8Array[] = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk) => written.push(chunk),
      () => 1,
      () => undefined,
    );
    const borrowed = Uint8Array.of(1, 2, 3);
    scheduler.enqueue(borrowed);
    borrowed.fill(9);
    expect([...written[0]]).toEqual([1, 2, 3]);
  });

  it("compacts a long consumed prefix without changing byte or callback order", () => {
    const frames: FrameRequestCallback[] = [];
    const completions: Array<() => void> = [];
    const written: number[] = [];
    const rendered: number[] = [];
    const measurements = new OperationCounters();
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => { written.push(...chunk); completions.push(done); },
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      64,
      1024,
      undefined,
      undefined,
      measurements,
    );
    for (let index = 0; index < 131; index += 1) {
      scheduler.enqueue(Uint8Array.of(index), () => rendered.push(index));
    }
    let ticks = 0;
    while (scheduler.pendingBytes > 0 && ticks < 100) {
      while (completions.length) completions.shift()!();
      for (const frame of frames.splice(0, frames.length)) frame(ticks * 16);
      ticks += 1;
    }
    while (completions.length) completions.shift()!();
    expect(written).toEqual(Array.from({ length: 131 }, (_, index) => index & 0xff));
    expect(rendered).toEqual(Array.from({ length: 131 }, (_, index) => index));
    expect(measurements.snapshot().counters["terminal.scheduler.queueCompactions"]).toBeGreaterThan(0);
  });
});

describe("Phase 14 terminal operation fixture", () => {
  // Captured from the identical deterministic fixture on b082f66. Copy totals
  // include both the decoder's compact payload ownership and the legacy
  // scheduler/backlog copying; operations include queue work and array moves.
  const baseline = {
    64: { decoderCopiedBytes: 512, schedulerCopiedBytes: 960, arrayMoveOperations: 28, queueOperations: 16, callbacks: 8, xtermWrites: 2 },
    1024: { decoderCopiedBytes: 8192, schedulerCopiedBytes: 15_360, arrayMoveOperations: 28, queueOperations: 16, callbacks: 8, xtermWrites: 2 },
    65536: { decoderCopiedBytes: 524_288, schedulerCopiedBytes: 983_040, arrayMoveOperations: 28, queueOperations: 16, callbacks: 8, xtermWrites: 3 },
  } as const;

  it("reports exact event, fanout, copy, queue, callback and frame counts by chunk size", () => {
    for (const chunkBytes of [64, 1024, 64 * 1024]) {
      const measurements = new OperationCounters();
      const hub = new TerminalEventHub(undefined, {}, measurements);
      const frames: FrameRequestCallback[] = [];
      const completions: Array<() => void> = [];
      const output: Uint8Array[] = [];
      let callbacks = 0;
      const scheduler = new TerminalWriteScheduler(
        (chunk, done) => { output.push(chunk.slice()); completions.push(done); },
        (callback) => { frames.push(callback); return frames.length; },
        () => undefined,
        256 * 1024,
        8 * 1024 * 1024,
        undefined,
        undefined,
        measurements,
      );
      const expected = new Uint8Array(chunkBytes * 8);
      for (let index = 0; index < 8; index += 1) {
        const data = new Uint8Array(chunkBytes).fill(index);
        expected.set(data, index * chunkBytes);
        const wire = wireOutputFrame(index + 1, index + 1, data);
        hub.publish(decodeTerminalEvent(wire.buffer, measurements));
      }
      hub.subscribePane("%1", (event) => {
        if (event.kind === "output") scheduler.enqueueOwned(event.data, () => { callbacks += 1; });
      });
      let ticks = 0;
      while (scheduler.pendingBytes > 0 && ticks < 100) {
        while (completions.length) completions.shift()!();
        for (const frame of frames.splice(0, frames.length)) frame(ticks * 16);
        ticks += 1;
      }
      while (completions.length) completions.shift()!();
      expect(scheduler.pendingBytes).toBe(0);
      expect(callbacks).toBe(8);
      const joined = new Uint8Array(output.reduce((total, chunk) => total + chunk.byteLength, 0));
      let offset = 0;
      for (const chunk of output) { joined.set(chunk, offset); offset += chunk.byteLength; }
      expect(joined.byteLength).toBe(expected.byteLength);
      expect(joined.every((byte, index) => byte === expected[index])).toBe(true);
      const snapshot = measurements.snapshot();
      expect(snapshot.counters["terminal.hub.events"]).toBe(8);
      expect(snapshot.counters["terminal.decoder.frames"]).toBe(8);
      expect(snapshot.counters["terminal.decoder.copiedBytes"]).toBe(chunkBytes * 8);
      expect(snapshot.counters["terminal.scheduler.enqueueOperations"]).toBe(8);
      expect(snapshot.counters["terminal.scheduler.dequeueOperations"]).toBe(8);
      expect(snapshot.counters["terminal.scheduler.callbacksInvoked"]).toBe(8);
      const reference = baseline[chunkBytes as keyof typeof baseline];
      expect(reference).toBeDefined();
      const copyProxyBytes = snapshot.counters["terminal.decoder.copiedBytes"]
        + (snapshot.counters["terminal.scheduler.copiedBytes"] ?? 0);
      const baselineCopyProxyBytes = reference.decoderCopiedBytes + reference.schedulerCopiedBytes;
      const operationProxy = snapshot.counters["terminal.scheduler.enqueueOperations"]
        + snapshot.counters["terminal.scheduler.dequeueOperations"];
      const baselineOperationProxy = reference.queueOperations + reference.arrayMoveOperations;
      expect(snapshot.counters["terminal.scheduler.callbacksInvoked"]).toBe(reference.callbacks);
      expect(snapshot.counters["terminal.scheduler.xtermWrites"]).toBe(reference.xtermWrites);
      expect(copyProxyBytes / baselineCopyProxyBytes).toBeLessThanOrEqual(0.7);
      expect(operationProxy / baselineOperationProxy).toBeLessThanOrEqual(0.7);
      console.log(`PHASE14_METRIC ${JSON.stringify({
        lane: "terminalFrontend", chunkBytes, eventCount: 8,
        byteExact: true, callbackExact: true,
        copyProxyBytes, baselineCopyProxyBytes,
        operationProxy, baselineOperationProxy,
        ...snapshot,
      })}`);
    }
  });
});

/**
 * Stage 12.9 item 3 measurement lane (P12-U002).
 *
 * An agent TUI repaint does not arrive as one write: tmux splits it across many
 * `%output`/`%extended-output` records, so the renderer sees K queued events for
 * one frame of screen. The number this lane reports is how many animation frames
 * pass before the last of those bytes — and the keystroke echo queued behind
 * them — reach xterm. It is the renderer-side half of the typing-lag budget, and
 * it is deterministic: the frame clock and the write completions are injected,
 * so the figure is a count, not a stopwatch reading.
 */
describe("agent-repaint frame cost", () => {
  const FRAME_MS = 1000 / 60;

  const measure = (eventCount: number, bytesPerEvent: number) => {
    const frames: FrameRequestCallback[] = [];
    const completions: Array<() => void> = [];
    const scheduler = new TerminalWriteScheduler(
      (_chunk, done) => completions.push(done),
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
      256 * 1024,
      8 * 1024 * 1024,
    );
    let echoReached = false;
    for (let index = 0; index < eventCount; index += 1) {
      scheduler.enqueue(new Uint8Array(bytesPerEvent));
    }
    // The user's keystroke echo is one small event queued behind the repaint.
    scheduler.enqueue(Uint8Array.of(0x61), () => { echoReached = true; });

    let framesElapsed = 0;
    while (!echoReached && framesElapsed < 500) {
      // xterm parses what it was handed before the next frame is serviced.
      while (completions.length) completions.shift()!();
      if (echoReached) break;
      const due = frames.splice(0, frames.length);
      if (due.length === 0) break;
      framesElapsed += 1;
      for (const callback of due) callback(framesElapsed * FRAME_MS);
    }
    while (completions.length) completions.shift()!();
    return { framesElapsed, echoReached, latencyMs: framesElapsed * FRAME_MS };
  };

  it("delivers a 4 KiB 24-event repaint and the echo behind it within one frame", () => {
    const repaint = measure(24, 170);
    const singleEcho = measure(0, 0);

    console.log(`agent-repaint frame cost: 24-event 4 KiB repaint + echo = ${repaint.framesElapsed} frames (${repaint.latencyMs.toFixed(1)} ms at 60 Hz); idle echo = ${singleEcho.framesElapsed} frames`);
    expect(repaint.echoReached).toBe(true);
    expect(singleEcho.framesElapsed).toBe(0);
    // One frame of budget for a whole repaint, not one frame per event.
    expect(repaint.framesElapsed).toBeLessThanOrEqual(1);
  });

  it("still paces a flood at the per-frame byte budget", () => {
    const flood = measure(8, 256 * 1024);

    console.log(`agent-repaint frame cost: 2 MiB flood + echo = ${flood.framesElapsed} frames (${flood.latencyMs.toFixed(1)} ms at 60 Hz)`);
    expect(flood.echoReached).toBe(true);
    expect(flood.framesElapsed).toBeGreaterThanOrEqual(7);
  });
});

describe("synchronized output holds", () => {
  it("never leaves a DEC 2026 bracket open for more than the frame that closed it", async () => {
    // xterm force-clears a synchronized-output hold 1000 ms after the first
    // held refresh and repaints a half-applied frame — the mid-word tearing in
    // P12-U003.2. What delayed the closing bracket was the scheduler, so the
    // check is that a bracketed repaint split across many output events closes
    // inside one frame.
    const terminal = new HeadlessTerminal({ cols: 40, rows: 8, allowProposedApi: true });
    const frames: FrameRequestCallback[] = [];
    const scheduler = new TerminalWriteScheduler(
      (chunk, done) => terminal.write(chunk, done),
      (callback) => { frames.push(callback); return frames.length; },
      () => undefined,
    );
    const encoder = new TextEncoder();
    const events = [encoder.encode("\u001b[?2026h")];
    for (let row = 1; row <= 6; row += 1) events.push(encoder.encode(`\u001b[${row};1H` + "x".repeat(30)));
    events.push(encoder.encode("\u001b[?2026l"));
    for (const event of events) scheduler.enqueue(event);

    const settle = () => new Promise<void>((resolve) => terminal.write("", resolve));
    await settle();
    expect(terminal.modes.synchronizedOutputMode).toBe(true);
    for (const frame of frames.splice(0, frames.length)) frame(16);
    await settle();
    expect(terminal.modes.synchronizedOutputMode).toBe(false);
    expect(scheduler.pendingBytes).toBe(0);
    terminal.dispose();
  });
});

describe("serialize/restore attribute parity", () => {
  it("round-trips bold, dim and colour through the snapshot the hide handoff sends", async () => {
    // Every hide snapshot and every cached restore flows through this addon.
    // Until this upgrade the installed copy declared a peer of xterm ^5 against
    // an installed 6.0.0 while reaching into private internals, which is the
    // suspected source of the bold/dim wrongness reported after a reveal
    // (P12-U003.5). The check is a full-cell comparison, not a text one.
    const write = (terminal: HeadlessTerminal, value: string) =>
      new Promise<void>((resolve) => terminal.write(value, resolve));
    const source = new HeadlessTerminal({ cols: 40, rows: 4, allowProposedApi: true });
    const serialize = new SerializeAddon();
    source.loadAddon(serialize as unknown as Parameters<HeadlessTerminal["loadAddon"]>[0]);
    await write(source, "\u001b[1mWor\u001b[22m\u001b[2mking\u001b[0m \u001b[31mred\u001b[39m λ🚀\r\nplain");

    const restored = new HeadlessTerminal({ cols: 40, rows: 4, allowProposedApi: true });
    await write(restored, serialize.serialize({ scrollback: 0 }));

    for (let row = 0; row < 4; row += 1) {
      const before = source.buffer.active.getLine(row);
      const after = restored.buffer.active.getLine(row);
      expect(after?.translateToString(true)).toBe(before?.translateToString(true));
      for (let column = 0; column < 40; column += 1) {
        const cell = before?.getCell(column);
        const copy = after?.getCell(column);
        expect(`${row}:${column} ${copy?.getChars()}/${copy?.isBold()}/${copy?.isDim()}/${copy?.getFgColor()}/${copy?.getBgColor()}`)
          .toBe(`${row}:${column} ${cell?.getChars()}/${cell?.isBold()}/${cell?.isDim()}/${cell?.getFgColor()}/${cell?.getBgColor()}`);
      }
    }
    source.dispose();
    restored.dispose();
  });
});

describe("restore admission", () => {
  it("recovers from the host instead of replacing newer output with an older screen", () => {
    expect(restoreDecision(5, 5, false)).toEqual({ kind: "apply" });
    expect(restoreDecision(9, 5, false)).toEqual({ kind: "apply" });
    expect(restoreDecision(4, 5, false).kind).toBe("reseed");
    // An overflowed pane owes the host a seed; a cached screen is not one, and
    // silently doing nothing marks the pane ready while it shows nothing.
    expect(restoreDecision(9, 5, true).kind).toBe("reseed");
  });
});

describe("pane grid reconciliation", () => {
  const pane = { id: "%2", width: 49, height: 14 } as Pane;
  const recordingRenderer = (outcome: (size: TerminalSize) => GridOutcome) => {
    const applied: TerminalSize[] = [];
    return {
      applied,
      renderer: { setGrid: (size: TerminalSize) => { applied.push(size); return outcome(size); } },
    };
  };

  it("renders at tmux's grid, not at the grid its CSS box measured", () => {
    const { applied, renderer } = recordingRenderer((size) => ({ kind: "applied", size }));
    const report = reconcilePaneGrid(renderer, pane, { columns: 50, rows: 15 });
    expect(applied).toEqual([{ columns: 49, rows: 14 }]);
    expect(report).toContain("tmux reports 49x14");
  });

  it("says nothing when the box already agreed with tmux", () => {
    const { renderer } = recordingRenderer(() => ({ kind: "unchanged" }));
    expect(reconcilePaneGrid(renderer, pane, { columns: 49, rows: 14 })).toBeUndefined();
  });

  it("settles: reconciling twice resizes once and then says nothing", () => {
    // Every reconcile is driven by something that can be caused by a resize —
    // a ResizeObserver callback, a topology push. If applying tmux's grid could
    // provoke another apply, the pane would resize in a loop and repaint on
    // every frame, which is what continuous flickering is. `measure()` is
    // propose-only and `setGrid` is the only writer, so the second call has
    // nothing to do.
    let grid: TerminalSize = { columns: 80, rows: 24 };
    const resizes: TerminalSize[] = [];
    const renderer = {
      setGrid: (size: TerminalSize): GridOutcome => {
        if (size.columns < 2 || size.rows < 2) return { kind: "rejected", reason: "unusable" };
        if (grid.columns === size.columns && grid.rows === size.rows) return { kind: "unchanged" };
        grid = size;
        resizes.push(size);
        return { kind: "applied", size };
      },
    };
    const measured = { columns: 50, rows: 15 };
    expect(reconcilePaneGrid(renderer, pane, measured)).toContain("tmux reports 49x14");
    expect(reconcilePaneGrid(renderer, pane, measured)).toBeUndefined();
    expect(reconcilePaneGrid(renderer, pane, measured)).toBeUndefined();
    expect(resizes).toEqual([{ columns: 49, rows: 14 }]);
  });

  it("falls back to the measured box only when tmux reports no usable grid", () => {
    const { applied, renderer } = recordingRenderer((size) =>
      size.columns < 2 ? { kind: "rejected", reason: "0x0 is not a usable terminal grid" } : { kind: "applied", size });
    const report = reconcilePaneGrid(renderer, { ...pane, width: 0, height: 0 } as Pane, { columns: 50, rows: 15 });
    expect(applied.at(-1)).toEqual({ columns: 50, rows: 15 });
    expect(report).toContain("no usable grid");
  });
});

describe("SearchAddon integration", () => {
  const write = (terminal: HeadlessTerminal, value: string) => new Promise<void>((resolve) => terminal.write(value, resolve));
  const searchableTerminal = (options: ConstructorParameters<typeof HeadlessTerminal>[0]) => {
    const terminal = new HeadlessTerminal({ ...options, allowProposedApi: true });
    let selection: { start: { x: number; y: number }; end: { x: number; y: number } } | undefined;
    Object.assign(terminal, {
      getSelectionPosition: () => selection,
      clearSelection: () => { selection = undefined; },
      select: (column: number, row: number, length: number) => {
        selection = { start: { x: column, y: row }, end: { x: column + length, y: row } };
      },
    });
    return { terminal, selection: () => selection };
  };

  it("cycles repeated matches and wraps to the first result", async () => {
    const { terminal, selection } = searchableTerminal({ cols: 20, rows: 4 });
    const search = new SearchAddon(); terminal.loadAddon(search as unknown as Parameters<HeadlessTerminal["loadAddon"]>[0]);
    await write(terminal, "one one");
    expect(search.findNext("one")).toBe(true);
    const first = selection();
    expect(search.findNext("one")).toBe(true);
    expect(selection()).not.toEqual(first);
    expect(search.findNext("one")).toBe(true);
    expect(selection()).toEqual(first);
    terminal.dispose();
  });

  it("searches wrapped rows, CJK cells, and combining sequences", async () => {
    const { terminal } = searchableTerminal({ cols: 5, rows: 5 });
    const search = new SearchAddon(); terminal.loadAddon(search as unknown as Parameters<HeadlessTerminal["loadAddon"]>[0]);
    await write(terminal, "abcdef\r\n漢字\r\ne\u0301");
    expect(search.findNext("cdef")).toBe(true);
    expect(search.findNext("漢字")).toBe(true);
    expect(search.findNext("e\u0301")).toBe(true);
    terminal.dispose();
  });
});

describe("local terminal selection modifier", () => {
  it("uses Shift to force local selection while Linux TUIs report mouse input", () => {
    expect(isForcedLocalSelection({ shiftKey: true })).toBe(true);
    expect(isForcedLocalSelection({ shiftKey: false })).toBe(false);
  });
});

describe("native terminal paste interception", () => {
  it("owns plain text before xterm can add a second bracketed-paste envelope", () => {
    const paste = vi.fn();
    const event = {
      clipboardData: { getData: (type: string) => type === "text/plain" ? "printf 'rocket 🚀\\n'" : "" },
      defaultPrevented: false,
      preventDefault: vi.fn(),
      stopImmediatePropagation: vi.fn(),
      stopPropagation: vi.fn(),
    };
    expect(interceptTerminalPlainTextPaste(event, paste)).toBe(true);
    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(event.stopImmediatePropagation).toHaveBeenCalledOnce();
    expect(paste).toHaveBeenCalledWith("printf 'rocket 🚀\\n'");
  });

  it("leaves file or image paste already claimed by the transfer surface alone", () => {
    const paste = vi.fn();
    const event = {
      clipboardData: { getData: () => "file:///tmp/image.png" },
      defaultPrevented: true,
      preventDefault: vi.fn(),
      stopImmediatePropagation: vi.fn(),
      stopPropagation: vi.fn(),
    };
    expect(interceptTerminalPlainTextPaste(event, paste)).toBe(false);
    expect(paste).not.toHaveBeenCalled();
  });
});

describe("pane resource recovery", () => {
  const resource = {
    kind: "paneResource", paneId: "%1", state: "hiddenBuffered", requiresSeed: false,
    recoveryReason: "", generation: 2, snapshotGeneration: 1, tailThroughGeneration: 2,
    serializedSnapshot: copyTerminalBytes(new TextEncoder().encode("screen λ")),
    rawTail: copyTerminalBytes(Uint8Array.of(27, 91, 109)),
    sequence: 2,
  } as const;

  it("restores a valid serialized snapshot before its byte-exact raw tail", () => {
    expect(paneRecoveryPlan(resource)).toEqual({
      kind: "restore", serialized: "screen λ", rawTail: Uint8Array.of(27, 91, 109),
      snapshotGeneration: 1, tailThroughGeneration: 2,
    });
  });

  it("awaits a seed for released or malformed recovery state", () => {
    expect(paneRecoveryPlan({ ...resource, state: "released", requiresSeed: true, recoveryReason: "evicted" }))
      .toEqual({ kind: "awaitSeed", reason: "evicted" });
    expect(paneRecoveryPlan({ ...resource, serializedSnapshot: copyTerminalBytes(Uint8Array.of(0xff)) }).kind).toBe("awaitSeed");
  });
});
