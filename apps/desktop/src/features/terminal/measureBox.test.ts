// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { cellsForBox, deviceSafeLineHeight, terminalMeasurements } from "./TerminalRenderer";

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
    expect(
      charSize(terminal),
      "xterm moved _core._charSizeService; the renderer reads .height and .onCharSizeChange",
    ).toBeDefined();
    expect(
      charSize(terminal)?.onCharSizeChange,
      "xterm moved _core._charSizeService.onCharSizeChange; line-height refits would silently stop",
    ).toBeTypeOf("function");
    expect(
      dimensionsChangeEvent(terminal),
      "xterm moved _core._renderService.onDimensionsChange; DPR-driven refits would silently stop",
    ).toBeTypeOf("function");
    terminal.dispose();
  });

  it("keeps a row whole in both pixel spaces for at most a CSS pixel of pitch", () => {
    // Judged by what xterm would then render — it floors the multiplier into
    // the measured character — rather than by the multiplier itself.
    const renderedDeviceRow = (charHeight: number, ratio: number) => {
      const deviceChar = Math.ceil(charHeight * ratio);
      return Math.floor(deviceChar * deviceSafeLineHeight(charHeight, ratio)!);
    };
    // Two properties, swept over the ratios a display can report and the faces
    // a monospace stack can measure.
    for (const ratio of [1, 1.25, 1.5, 1.75, 2, 2.5, 3]) {
      for (const charHeight of [12, 15, 16.4, 17, 17.6, 18, 18.3, 19.2, 24]) {
        const deviceChar = Math.ceil(charHeight * ratio);
        const deviceRow = renderedDeviceRow(charHeight, ratio);
        const label = `${ratio}x, ${charHeight}px face`;
        // Never below the face: xterm refuses a multiplier under 1, and the row
        // that clamp produces is a number nothing chose.
        expect(deviceRow, label).toBeGreaterThanOrEqual(deviceChar);
        // The budget. It is stated in the unit the app loses when it is
        // exceeded — a pitch dragged further costs whole rows of terminal, and
        // tmux is sized from the count.
        expect((deviceRow - deviceChar) / ratio, `${label} pitch sacrifice`).toBeLessThanOrEqual(1);
      }
    }
    // A 17px face, ratio by ratio: a whole row inside the budget is taken, and
    // then `rows × cell` divides by the ratio exactly at every grid size.
    for (const ratio of [1, 1.5, 2, 2.5, 3]) {
      const deviceRow = renderedDeviceRow(17, ratio);
      expect(Number.isInteger(deviceRow / ratio), `${ratio}x CSS row`).toBe(true);
      for (const rows of [1, 7, 38, 39, 40, 53]) {
        expect(Number.isInteger(rows * deviceRow / ratio), `${ratio}x at ${rows} rows`).toBe(true);
      }
    }
    // And the documented refusal, so that changing the budget has to change
    // this test and say why. At 1.25 the nearest row whole in both spaces is a
    // multiple of 4 CSS px, which would drag a 17.6px native pitch to 20: an
    // 800px pane would show 40 rows where 45 fit, and tmux would be told 40.
    expect(renderedDeviceRow(17, 1.25)).toBe(22);
    expect(Math.floor(800 / (22 / 1.25))).toBe(45);
    // 1.75 refuses for the same reason: 18 CSS px is 31.5 device px.
    expect(renderedDeviceRow(17, 1.75)).toBe(30);
    // Nothing measured, nothing derived: the caller leaves xterm alone.
    expect(deviceSafeLineHeight(undefined, 2)).toBeUndefined();
    expect(deviceSafeLineHeight(0, 2)).toBeUndefined();
    // A nonsense ratio falls back to 1 rather than producing a nonsense cell.
    expect(deviceSafeLineHeight(17, 0)).toBe(deviceSafeLineHeight(17, 1));
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

function charSize(terminal: Terminal): { height?: number; onCharSizeChange?: unknown } | undefined {
  return (terminal as unknown as {
    _core?: { _charSizeService?: { height?: number; onCharSizeChange?: unknown } };
  })._core?._charSizeService;
}

function dimensionsChangeEvent(terminal: Terminal): unknown {
  return (terminal as unknown as {
    _core?: { _renderService?: { onDimensionsChange?: unknown } };
  })._core?._renderService?.onDimensionsChange;
}
