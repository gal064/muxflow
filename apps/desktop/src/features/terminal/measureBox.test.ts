// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { cellsForBox } from "./TerminalRenderer";

/**
 * The tmux client size is derived from cell metrics xterm does not expose
 * publicly, so `XtermRenderer.measureBox` reads the same internal render
 * service `FitAddon.proposeDimensions` reads. These are the canary for that:
 * they fail on the xterm upgrade that moves it or changes its arithmetic,
 * rather than letting the app silently stop sizing its tmux client — the quiet
 * direction of P12-U006.
 */
describe("xterm cell metrics", () => {
  // jsdom has no matchMedia, which xterm's DPR watcher requires on open().
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

  it("still live where both FitAddon and measureBox read them", () => {
    const terminal = new Terminal();
    terminal.open(document.createElement("div"));
    expect(
      cellSize(terminal),
      "xterm moved _core._renderService.dimensions.css.cell; measureBox reads it",
    ).toBeDefined();
    terminal.dispose();
  });

  it("produce the same cells as FitAddon for the same box", () => {
    const host = document.createElement("div");
    host.style.width = "1000px";
    host.style.height = "800px";
    document.body.append(host);
    const terminal = new Terminal({ scrollback: 10_000 });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(host);
    // jsdom measures no glyphs, so the render service holds a zero cell; supply
    // one and let both computations run off it.
    const cell = { width: 8, height: 17 };
    Object.assign(cellSize(terminal)!, cell);

    // `measureBox`'s arithmetic, with the chrome an unpadded host spends: none
    // of its own, and xterm's scrollbar allowance.
    const mine = cellsForBox({ width: 1000, height: 800 }, cell, { horizontal: 0, vertical: 0, scrollbar: 14 });
    const theirs = fit.proposeDimensions();
    expect(mine).toEqual({ columns: theirs!.cols, rows: theirs!.rows });
    expect(mine).toEqual({ columns: 123, rows: 47 });
    terminal.dispose();
    host.remove();
  });
});

function cellSize(terminal: Terminal): { width: number; height: number } | undefined {
  return (terminal as unknown as {
    _core?: { _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } } };
  })._core?._renderService?.dimensions?.css?.cell;
}
