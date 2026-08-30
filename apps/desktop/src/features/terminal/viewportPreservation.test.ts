import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import {
  captureTerminalViewport,
  resizeTerminalPreservingViewport,
  restoreTerminalViewport,
  type TerminalViewportAnchor,
} from "./terminalViewport";

async function writeLines(terminal: Terminal, count = 40): Promise<void> {
  // Thirty printable cells fit at 40 columns and wrap at 20. Every row before
  // the viewport therefore moves during the resize, which distinguishes a
  // logical marker from merely retaining the old numeric viewportY.
  const output = Array.from(
    { length: count },
    (_, index) => `row-${String(index).padStart(2, "0")}-${"x".repeat(23)}`,
  ).join("\r\n");
  await new Promise<void>((resolve) => terminal.write(output, resolve));
}

describe("terminal viewport preservation", () => {
  it("moves a middle-of-history anchor with wrapped-line reflow", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 40, rows: 5, scrollback: 1_000 });
    await writeLines(terminal);
    const before: TerminalViewportAnchor = {
      atBottom: false,
      viewportLine: 15,
      grid: { columns: 40, rows: 5 },
    };
    restoreTerminalViewport(terminal, before);
    const topLabel = terminal.buffer.active.getLine(before.viewportLine)?.translateToString(true).slice(0, 6);
    expect(topLabel).toBe("row-15");

    resizeTerminalPreservingViewport(terminal, { columns: 20, rows: 5 });
    const after = captureTerminalViewport(terminal);

    expect(after.atBottom).toBe(false);
    expect(after.viewportLine).toBeGreaterThan(before.viewportLine);
    expect(after.viewportLine).not.toBe(0);
    expect(after.grid).toEqual({ columns: 20, rows: 5 });
    expect(terminal.buffer.active.getLine(after.viewportLine)?.translateToString(true)).toMatch(/^row-15/);

    resizeTerminalPreservingViewport(terminal, { columns: 40, rows: 5 });
    const widened = captureTerminalViewport(terminal);
    expect(widened.viewportLine).toBe(before.viewportLine);
    expect(terminal.buffer.active.getLine(widened.viewportLine)?.translateToString(true)).toMatch(/^row-15/);
    terminal.dispose();
  });

  it("keeps a bottom-following pane at the bottom through reflow", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 40, rows: 5, scrollback: 1_000 });
    await writeLines(terminal);

    resizeTerminalPreservingViewport(terminal, { columns: 20, rows: 8 });
    expect(captureTerminalViewport(terminal)).toMatchObject({
      atBottom: true,
      grid: { columns: 20, rows: 8 },
    });
    terminal.dispose();
  });

  it("restores a cached line only at the grid that produced it and otherwise falls back to bottom", async () => {
    const exact = new Terminal({ allowProposedApi: true, cols: 40, rows: 5, scrollback: 1_000 });
    await writeLines(exact);
    restoreTerminalViewport(exact, { atBottom: false, viewportLine: 12, grid: { columns: 40, rows: 5 } });
    expect(captureTerminalViewport(exact)).toEqual({
      atBottom: false,
      viewportLine: 12,
      grid: { columns: 40, rows: 5 },
    });
    exact.dispose();

    const mismatched = new Terminal({ allowProposedApi: true, cols: 40, rows: 5, scrollback: 1_000 });
    await writeLines(mismatched);
    restoreTerminalViewport(mismatched, { atBottom: false, viewportLine: 12, grid: { columns: 80, rows: 24 } });
    expect(captureTerminalViewport(mismatched).atBottom).toBe(true);
    mismatched.dispose();
  });
});
