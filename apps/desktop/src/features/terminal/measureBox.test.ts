// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { cellsForBox, terminalMeasurements, xtermLineHeight } from "./TerminalRenderer";

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

  it("still expose the measured character height the row pitch is derived from", () => {
    const terminal = new Terminal();
    terminal.open(document.createElement("div"));
    expect(
      charSizeService(terminal)?.height,
      "xterm moved _core._charSizeService.height; the terminal's row pitch is derived from it",
    ).toBeTypeOf("number");
    expect(
      charSizeService(terminal)?.onCharSizeChange,
      "xterm moved _core._charSizeService.onCharSizeChange; the row pitch re-derives on it",
    ).toBeTypeOf("function");
    terminal.dispose();
  });

  it("turns a CSS row pitch into the multiplier xterm actually applies", () => {
    // A row is a whole number of device pixels and xterm floors into them, so
    // the answer is judged by what xterm would then render, not by the ratio.
    const rendered = (pitch: number, charHeight: number, ratio: number) =>
      Math.floor(Math.ceil(charHeight * ratio) * xtermLineHeight(pitch, charHeight, ratio)!) / ratio;
    // 13px JetBrains Mono measures ~17px, so a 1.42 CSS line-height is a ~1.07
    // xterm one — and handing xterm 1.42 rendered ~24px rows instead.
    expect(xtermLineHeight(13 * 1.42, 17, 2)).toBeCloseTo(1.0735, 3);
    // 18.46 CSS px is 36.92 device px. The nearest device row, 37, is odd, and
    // an odd device row makes the WebGL canvas's backing store and its CSS box
    // disagree by a pixel at every odd row count — which stretches the whole
    // grid. A whole CSS pixel is the constraint, so 18.0 it is, at any ratio.
    expect(rendered(13 * 1.42, 17, 2)).toBe(18);
    expect(rendered(13 * 1.42, 17, 1)).toBe(18);
    // 1.5 has a whole row too — any even one — and 18 CSS px is 27 device px,
    // which divides exactly at every row count.
    expect(rendered(13 * 1.42, 17, 1.5)).toBe(18);
    // Two properties, swept over the ratios a display can report and the faces a
    // 13px monospace stack can measure.
    for (const ratio of [1, 1.25, 1.5, 2, 3]) {
      for (const charHeight of [12, 15, 16.4, 17, 17.6, 18, 18.3, 19.2, 24]) {
        const row = rendered(13 * 1.42, charHeight, ratio);
        const label = `ratio ${ratio}, char ${charHeight} rendered a ${row}px row`;
        // Unconditional: the row fits the face, so xterm's own below-1 clamp
        // never takes over and hands back a height nothing chose.
        expect(row * ratio, label).toBeGreaterThanOrEqual(Math.ceil(charHeight * ratio));
        expect(xtermLineHeight(13 * 1.42, charHeight, ratio), label).toBeGreaterThanOrEqual(1);
        // Where the app actually lives — a face shorter than the pitch, on a
        // ratio with a cheap whole row — `rows × cell` divides by the ratio
        // exactly, however many rows the window ends up with.
        // The face after xterm's own ceil, which is what has to fit under the
        // pitch for a cheap whole row to exist at all.
        if (ratio !== 1.25 && Math.ceil(charHeight * ratio) / ratio <= 13 * 1.42) {
          expect(Number.isInteger(row), label).toBe(true);
          for (const rows of [1, 7, 39, 40, 53]) {
            expect(Number.isInteger(rows * row * ratio), `${label} at ${rows} rows`).toBe(true);
          }
        }
      }
    }
    // The two documented places the snap is refused, both deliberate, so that
    // changing the budget has to change this test and say why.
    //
    // 1.25 needs a multiple of 4 CSS px to be whole in both spaces, which would
    // drag an 18.46px pitch to 20 — 8% of the design, worse than the stretch.
    expect(rendered(13 * 1.42, 17, 1.25)).toBe(23 / 1.25);
    // And a face so tall that the shortest whole row fitting it is 20 CSS px,
    // for the same reason. The row still fits the face, asserted above.
    expect(rendered(13 * 1.42, 18.3, 1.5)).toBe(28 / 1.5);
    // Nothing measured, nothing derived: the caller leaves xterm alone.
    expect(xtermLineHeight(18.46, undefined)).toBeUndefined();
    expect(xtermLineHeight(18.46, 0)).toBeUndefined();
    expect(xtermLineHeight(0, 17.05)).toBeUndefined();
    // A nonsense ratio falls back to 1 rather than producing a nonsense cell.
    expect(xtermLineHeight(13 * 1.42, 17, 0)).toBe(xtermLineHeight(13 * 1.42, 17, 1));
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

function charSizeService(terminal: Terminal): { height?: number; onCharSizeChange?: unknown } | undefined {
  return (terminal as unknown as {
    _core?: { _charSizeService?: { height?: number; onCharSizeChange?: unknown } };
  })._core?._charSizeService;
}
