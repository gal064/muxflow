import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";

const write = (terminal: Terminal, value: string) =>
  new Promise<void>((resolve) => terminal.write(value, resolve));

describe("terminal seed cell overlays", () => {
  it("fills an attributed erase through the right edge", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 20, rows: 3 });

    // The joined capture supplies the text; the physical-cell overlay supplies
    // only the SGR state, its authoritative start column, and erase-to-end.
    await write(
      terminal,
      "\u001b[48;2;65;69;76mPlan\u001b[m"
        + "\u001b[48;2;65;69;76m\u001b[1;5H\u001b[K\u001b[m",
    );

    const line = terminal.buffer.active.getLine(0);
    expect(line?.translateToString(true)).toBe("Plan");
    for (let column = 0; column < 20; column += 1) {
      expect(line?.getCell(column)?.getBgColor()).toBe(0x41454c);
    }
    terminal.dispose();
  });

  it("does not erase logical soft-wrap metadata", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 20, rows: 3 });
    await write(terminal, "1234567890123456789012345");
    expect(terminal.buffer.active.getLine(1)?.isWrapped).toBe(true);

    await write(
      terminal,
      "\u001b[m\u001b[48;2;65;69;76m\u001b[2;6H\u001b[K\u001b[m",
    );

    expect(terminal.buffer.active.getLine(1)?.isWrapped).toBe(true);
    terminal.dispose();
  });

  it("restores adjacent blank spans with different backgrounds", async () => {
    const terminal = new Terminal({ allowProposedApi: true, cols: 20, rows: 3 });
    await write(
      terminal,
      "\u001b[41mA\u001b[m"
        + "\u001b[41m\u001b[1;2H\u001b[9X"
        + "\u001b[44m\u001b[1;11H\u001b[K\u001b[m",
    );

    const line = terminal.buffer.active.getLine(0);
    for (let column = 0; column < 10; column += 1) {
      expect(line?.getCell(column)?.getBgColor()).toBe(1);
    }
    for (let column = 10; column < 20; column += 1) {
      expect(line?.getCell(column)?.getBgColor()).toBe(4);
    }
    terminal.dispose();
  });
});
