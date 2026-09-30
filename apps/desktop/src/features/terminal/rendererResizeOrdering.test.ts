// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { XtermRenderer } from "./TerminalRenderer";
import { ownTerminalBytes } from "./TerminalBytes";

const bytes = (value: string) => ownTerminalBytes(new TextEncoder().encode(value));

beforeAll(() => {
  window.matchMedia ??= ((query: string) => ({
    matches: false, media: query, onchange: null,
    addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
});

describe("renderer resize ordering", () => {
  it.each(["seed", "restore"] as const)("keeps the latest grid when %s replaces a pending resize", async (kind) => {
    const actual = new XtermRenderer();
    const expected = new XtermRenderer();
    actual.open(document.createElement("div"));
    expected.open(document.createElement("div"));
    const snapshot = "\x1b[2J\x1b[H" + "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".repeat(2);
    try {
      actual.write(bytes("old output"), undefined, 1);
      actual.setGrid({ columns: 40, rows: 12 });
      expected.setGrid({ columns: 40, rows: 12 });
      for (const renderer of [actual, expected]) {
        await new Promise<void>((resolve) => {
          if (kind === "seed") renderer.seed(bytes(snapshot), resolve, 2);
          else expect(renderer.restore(snapshot, resolve, 2, 2)).toBe(true);
        });
      }
      expect(actual.grid).toEqual(expected.grid);
      expect(actual.screenText()).toEqual(expected.screenText());
    } finally {
      actual.dispose();
      expected.dispose();
    }
  });

  it("preserves intermediate sizes and writes when a split closes before queued output finishes", async () => {
    const actual = new XtermRenderer();
    const expected = new XtermRenderer();
    actual.open(document.createElement("div"));
    expected.open(document.createElement("div"));
    const steps = [
      { columns: 80, rows: 24, text: "\x1b[2;60HBEFORE SPLIT" },
      { columns: 40, rows: 12, text: "\x1b[3;30HDURING SPLIT" },
      { columns: 80, rows: 24, text: "\x1b[4;60HAFTER CLOSE" },
    ];
    try {
      for (const [index, step] of steps.entries()) {
        expected.setGrid(step);
        await new Promise<void>((resolve) => expected.write(bytes(step.text), resolve, index + 1));
      }
      for (const [index, step] of steps.entries()) {
        actual.setGrid(step);
        actual.write(bytes(step.text), undefined, index + 1);
      }
      const settled = await actual.drainAndSerialize();
      expect(settled.viewport.grid).toEqual({ columns: 80, rows: 24 });
      expect(actual.screenText()).toEqual(expected.screenText());
    } finally {
      actual.dispose();
      expected.dispose();
    }
  });

  it.each([false, true])("does not resize ahead of accepted output (alternate=%s)", async (alternate) => {
    const actual = new XtermRenderer();
    const expected = new XtermRenderer();
    actual.open(document.createElement("div"));
    expected.open(document.createElement("div"));
    const initial = (alternate ? "\x1b[?1049h" : "") + "\x1b[2J\x1b[HInitial screen";
    const beforeResize = "\x1b[2;60HSTATUS: READY\x1b[4;1H" + "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".repeat(2);
    const afterResize = "\x1b[5;1HNEXT ROW\x1b[K";
    try {
      await Promise.all([actual, expected].map((renderer) =>
        new Promise<void>((resolve) => renderer.write(bytes(initial), resolve, 1))));
      await new Promise<void>((resolve) => expected.write(bytes(beforeResize), resolve, 2));
      expected.setGrid({ columns: 40, rows: 24 });
      await new Promise<void>((resolve) => expected.write(bytes(afterResize), resolve, 3));

      // setGrid must not overtake either xterm's asynchronous parser or the
      // scheduler's own pending bytes. The next write belongs to the new grid.
      actual.write(bytes(beforeResize), undefined, 2);
      actual.setGrid({ columns: 40, rows: 24 });
      actual.write(bytes(afterResize), undefined, 3);
      const settled = await actual.drainAndSerialize();
      expect(actual.screenText()).toEqual(expected.screenText());
      expect(settled.viewport.grid).toEqual({ columns: 40, rows: 24 });
    } finally {
      actual.dispose();
      expected.dispose();
    }
  });
});
