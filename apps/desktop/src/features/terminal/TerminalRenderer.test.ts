import { describe, expect, it, vi } from "vitest";
import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { SearchAddon } from "@xterm/addon-search";
import { TerminalWriteScheduler } from "./TerminalRenderer";
import { interceptTerminalPlainTextPaste, isForcedLocalSelection, paneRecoveryPlan } from "./TerminalPane";

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
    frames.shift()!(0);
    expect(written).toEqual([[1, 2, 3]]);
    expect(scheduler.pendingBytes).toBe(6);
    expect(frames).toHaveLength(0);
    completions.shift()!();
    expect(scheduler.pendingBytes).toBe(3);
    frames.shift()!(16);
    expect(written.flat()).toEqual([1, 2, 3, 4]);
    completions.shift()!();
    frames.shift()!(32);
    completions.shift()!();
    expect(written.flat()).toEqual([1, 2, 3, 4, 5, 6]);
    expect(pending.at(-1)).toBe(0);
  });

  it("cancels and drops queued work on disposal", () => {
    let cancelled = 0;
    const completions: Array<() => void> = [];
    const scheduler = new TerminalWriteScheduler(
      (_chunk, done) => completions.push(done),
      () => 7,
      () => { cancelled += 1; },
    );
    scheduler.enqueue(Uint8Array.of(1));
    scheduler.enqueue(Uint8Array.of(2, 3));
    scheduler.dispose();
    scheduler.enqueue(Uint8Array.of(4));
    expect(cancelled).toBeGreaterThan(0);
    // The first byte took the idle fast path and is inside xterm's parser
    // already, so it cannot be un-sent; only the queued work is dropped.
    expect(scheduler.pendingBytes).toBe(1);
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
    frames.shift()!(32);
    completions.shift()!();
    expect(written.flat()).toEqual([1, 2, 3, 4, 0x1b, 0x63, 20, 21, 22]);
    expect(overflow).toEqual([11]);
    expect(scheduler.pendingBytes).toBe(0);
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
    serializedSnapshot: new TextEncoder().encode("screen λ"),
    rawTail: Uint8Array.of(27, 91, 109),
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
    expect(paneRecoveryPlan({ ...resource, serializedSnapshot: Uint8Array.of(0xff) }).kind).toBe("awaitSeed");
  });
});
