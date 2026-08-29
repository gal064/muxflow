// @vitest-environment jsdom
// The history capture runs without `-J`, so a page is physical rows and every
// number in the paging protocol is in the same unit. What `-J` used to buy —
// scrollback that reflows on a resize and copies as one line — is bought here
// instead, by composing the page so that xterm does the wrapping itself. These
// are the two halves of that: the arithmetic, and what a real xterm makes of
// the bytes it produces.
import { Terminal } from "@xterm/xterm";
import { beforeAll, describe, expect, it } from "vitest";
import { composeHistoryPage, splitHistoryRows, visibleWidth } from "./historyPage";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const rowsOf = (...text: string[]) => text.map((row) => encoder.encode(row));

beforeAll(() => {
  if (!window.matchMedia) {
    window.matchMedia = ((query: string) => ({
      matches: false, media: query, onchange: null,
      addEventListener: () => undefined, removeEventListener: () => undefined,
      addListener: () => undefined, removeListener: () => undefined,
      dispatchEvent: () => false,
    })) as typeof window.matchMedia;
  }
});

function terminalAt(columns: number, rows: number): Terminal {
  const terminal = new Terminal({ cols: columns, rows, scrollback: 1_000 });
  terminal.open(document.createElement("div"));
  return terminal;
}

/** Writes and waits for xterm's asynchronous parser to finish with it. */
async function write(terminal: Terminal, bytes: Uint8Array): Promise<void> {
  await new Promise<void>((resolve) => terminal.write(bytes, resolve));
}

describe("splitting a captured page", () => {
  // The host joins the captured rows and never appends a trailing separator, so
  // n separators are always n + 1 rows.
  it("counts n separators as n + 1 rows", () => {
    expect(splitHistoryRows(encoder.encode("a\r\nb\r\nc")).map((row) => decoder.decode(row)))
      .toEqual(["a", "b", "c"]);
    expect(splitHistoryRows(encoder.encode("only")).map((row) => decoder.decode(row)))
      .toEqual(["only"]);
  });

  // The row the old arithmetic dropped. A page whose last captured row is blank
  // ends on a separator, and reading that as "the separator closes the page"
  // loses a row the user printed — and with it, one row of every count taken
  // from the page after it.
  it("keeps a blank last row", () => {
    const rows = splitHistoryRows(encoder.encode("a\r\nb\r\n"));
    expect(rows.map((row) => decoder.decode(row))).toEqual(["a", "b", ""]);
  });
});

describe("measuring a captured row", () => {
  it("counts cells, not the SGR that colours them", () => {
    expect(visibleWidth(encoder.encode("plain"))).toBe(5);
    expect(visibleWidth(encoder.encode("\u001b[1;31mred\u001b[0m"))).toBe(3);
    // An OSC 8 hyperlink wraps the text it marks and takes no cells of its own.
    expect(visibleWidth(encoder.encode("\u001b]8;;http://x\u0007link\u001b]8;;\u0007"))).toBe(4);
  });
});

describe("composing a page for xterm", () => {
  it("breaks rows narrower than the grid and joins rows that fill it", () => {
    const composed = composeHistoryPage(rowsOf("x".repeat(8), "tail", "short"), 8);
    // The full-width row runs straight into its continuation; the short one is
    // broken. No trailing break: the caller separates the page from the screen.
    expect(decoder.decode(composed)).toBe(`${"x".repeat(8)}tail\r\nshort`);
  });

  // A blank row is output, and the `\r` that would carry a join cancels xterm's
  // pending wrap and swallows it. So a full-width row followed by a blank one is
  // broken, at the cost of a reflow this side cannot prove was ever wrapped.
  it("never joins a row into a blank one", () => {
    const composed = composeHistoryPage(rowsOf("x".repeat(8), "", "after"), 8);
    expect(decoder.decode(composed)).toBe(`${"x".repeat(8)}\r\n\r\nafter`);
  });

  /**
   * The whole reason the composition exists: xterm has to end up holding the
   * rows the way tmux held them, wrap flag included, or the scrollback this
   * splices in reflows and copies differently from the output printed live
   * beside it.
   */
  it("leaves xterm holding a wrapped row, not two hard-broken ones", async () => {
    const terminal = terminalAt(8, 4);
    await write(terminal, composeHistoryPage(rowsOf("abcdefgh", "ij", "next"), 8));

    const buffer = terminal.buffer.active;
    expect(buffer.getLine(0)?.translateToString(true)).toBe("abcdefgh");
    // The continuation carries xterm's own flag, which is what a resize reflows
    // on and what a selection reads to join the two.
    expect(buffer.getLine(1)?.isWrapped).toBe(true);
    expect(buffer.getLine(1)?.translateToString(true)).toBe("ij");
    // And the row after it is a row, not more of the same line.
    expect(buffer.getLine(2)?.isWrapped).toBe(false);

    terminal.selectLines(0, 1);
    expect(terminal.getSelection()).toBe("abcdefghij");
    terminal.dispose();
  });

  /**
   * And it costs exactly the rows it was captured with, whichever way each one
   * went. That count is what the splice scrolls back by to leave the user on
   * the rows they were reading.
   */
  it("occupies one terminal row per captured row", async () => {
    const captured = rowsOf("abcdefgh", "ij", "", "klm", "abcdefgh", "nop");
    const terminal = terminalAt(8, 3);
    await write(terminal, composeHistoryPage(captured, 8));

    // Six captured rows: the cursor sits on the last of them, and everything
    // above it is the other five.
    const buffer = terminal.buffer.active;
    expect(buffer.baseY + buffer.cursorY).toBe(captured.length - 1);
    terminal.dispose();
  });
});
