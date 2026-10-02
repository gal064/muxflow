// @vitest-environment jsdom
import { Terminal } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";
import { TerminalStream } from "./TerminalStream";

const bytes = (value: string) => new TextEncoder().encode(value);
function screen(term: Terminal): string[] {
  const buffer = term.buffer.active;
  return Array.from({ length: term.rows }, (_, row) => buffer.getLine(buffer.baseY + row)?.translateToString(true) ?? "");
}
const parse = (term: Terminal, value: string) => new Promise<void>((done) => term.write(bytes(value), done));

describe("mobile terminal stream ordering", () => {
  it("batches a burst of output without crossing a resize boundary", async () => {
    const term = new Terminal({ cols: 80, rows: 24 });
    const parseCalls = vi.spyOn(term, "write");
    const failed = vi.fn();
    const stream = new TerminalStream(term, vi.fn(), failed);
    try {
      stream.write(bytes("first"));
      stream.write(bytes(" second"));
      stream.resize({ cols: 40, rows: 12 });
      stream.write(bytes(" third"));
      stream.write(bytes(" fourth"));
      await stream.drain();
      expect(parseCalls).toHaveBeenCalledTimes(2);
      expect(screen(term)[0]).toBe("first second third fourth");
      expect(failed).not.toHaveBeenCalled();
    } finally { term.dispose(); }
  });
  it("renders a seed before immediately following live output", async () => {
    const term = new Terminal({ cols: 50, rows: 10 });
    const failed = vi.fn();
    const stream = new TerminalStream(term, vi.fn(), failed);
    try {
      stream.write(bytes("SCREEN SNAPSHOT"), true);
      stream.write(bytes("\r\nLIVE UPDATE"));
      await stream.drain();
      expect(screen(term).slice(0, 2)).toEqual(["SCREEN SNAPSHOT", "LIVE UPDATE"]);
      expect(failed).not.toHaveBeenCalled();
    } finally { term.dispose(); }
  });

  it.each([false, true])("parses each output at its confirmed host size (alternate=%s)", async (alternate) => {
    const actual = new Terminal({ cols: 80, rows: 24 });
    const expected = new Terminal({ cols: 80, rows: 24 });
    const failed = vi.fn();
    const stream = new TerminalStream(actual, vi.fn(), failed);
    const initial = (alternate ? "\x1b[?1049h" : "") + "\x1b[2J\x1b[H";
    const steps = [
      { cols: 80, rows: 24, text: initial + "\x1b[2;60HBEFORE RESIZE" },
      { cols: 40, rows: 12, text: "\x1b[3;30HDURING RESIZE" },
      { cols: 80, rows: 24, text: "\x1b[4;60HAFTER RESIZE" },
    ];
    try {
      for (const step of steps) {
        expected.resize(step.cols, step.rows);
        await parse(expected, step.text);
      }
      for (const step of steps) {
        stream.resize(step);
        stream.write(bytes(step.text));
      }
      await stream.drain();
      expect(screen(actual)).toEqual(screen(expected));
      expect(actual.cols).toBe(80);
      expect(actual.rows).toBe(24);
      expect(failed).not.toHaveBeenCalled();
    } finally { actual.dispose(); expected.dispose(); }
  });

  it("replays historical grid boundaries before subsequent live updates", async () => {
    const actual = new Terminal({ cols: 40, rows: 12 });
    const expected = new Terminal({ cols: 80, rows: 24 });
    const failed = vi.fn();
    const stream = new TerminalStream(actual, vi.fn(), failed);
    const before = "\x1b[2J\x1b[H\x1b[2;60HBEFORE RESIZE";
    const after = "\x1b[3;30HDURING RESIZE";
    const live = "\x1b[4;1HLIVE UPDATE";
    try {
      await parse(expected, "older\r\n" + "\r\n".repeat(24));
      await parse(expected, before);
      expected.resize(40, 12);
      await parse(expected, after);
      await parse(expected, live);
      stream.splice(bytes("older\r\n"), 1, bytes(before + after), [
        { offset: 0, cols: 80, rows: 24 },
        { offset: bytes(before).byteLength, cols: 40, rows: 12 },
      ]);
      stream.write(bytes(live));
      await stream.drain();
      expect(screen(actual)).toEqual(screen(expected));
      expect(failed).not.toHaveBeenCalled();
    } finally { actual.dispose(); expected.dispose(); }
  });
});
