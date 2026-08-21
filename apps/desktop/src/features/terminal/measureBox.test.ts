// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { cellsForBox, terminalMeasurements } from "./TerminalRenderer";

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
    expect(
      dprChangeEvent(terminal),
      "xterm moved _core._coreBrowserService.onDprChange; fixed boxes would miss DPR metric changes",
    ).toBeTypeOf("function");
    terminal.dispose();
  });

  it("are read into the same cells FitAddon computes, chrome and all", () => {
    // A host with padding and a border, so the chrome derivation is exercised
    // rather than reimplemented: `terminalMeasurements` reads it from the DOM
    // exactly where `FitAddon.proposeDimensions` reads its own.
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

    const measurements = terminalMeasurements(terminal, host, terminal.element!);
    expect(measurements).toEqual({
      cell,
      // An unpadded, unbordered host spends nothing; xterm still reserves its
      // scrollbar because this terminal has scrollback.
      chrome: { horizontal: 0, vertical: 0, scrollbar: 14 },
    });
    const mine = cellsForBox({ width: 1000, height: 800 }, measurements!.cell, measurements!.chrome);
    const theirs = fit.proposeDimensions();
    expect(mine).toEqual({ columns: theirs!.cols, rows: theirs!.rows });
    expect(mine).toEqual({ columns: 123, rows: 47 });

    // The host's own padding and border are part of what a terminal spends.
    host.style.padding = "6px";
    host.style.border = "1px solid black";
    expect(terminalMeasurements(terminal, host, terminal.element!)?.chrome).toEqual({
      horizontal: 14, vertical: 14, scrollbar: 14,
    });

    // A terminal with no scrollback reserves nothing for a scrollbar. (Changing
    // the option rebuilds xterm's buffers, so the stubbed cell is restated.)
    terminal.options.scrollback = 0;
    Object.assign(cellSize(terminal)!, cell);
    expect(terminalMeasurements(terminal, host, terminal.element!)?.chrome.scrollbar).toBe(0);

    // And a terminal whose metrics have gone reports nothing rather than a
    // number the app would resize somebody's tmux window with.
    Object.assign(cellSize(terminal)!, { width: 0, height: 0 });
    expect(terminalMeasurements(terminal, host, terminal.element!)).toBeUndefined();
    terminal.dispose();
    host.remove();
  });
});

function cellSize(terminal: Terminal): { width: number; height: number } | undefined {
  return (terminal as unknown as {
    _core?: { _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } } };
  })._core?._renderService?.dimensions?.css?.cell;
}

function dprChangeEvent(terminal: Terminal): unknown {
  return (terminal as unknown as {
    _core?: { _coreBrowserService?: { onDprChange?: unknown } };
  })._core?._coreBrowserService?.onDprChange;
}
