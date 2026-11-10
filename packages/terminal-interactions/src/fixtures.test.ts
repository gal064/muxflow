import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import bufferLinks from "../fixtures/buffer-links.json";
import fileLinks from "../fixtures/file-links.json";
import selectionCapture from "../fixtures/selection-capture.json";
import selectionClean from "../fixtures/selection-clean.json";
import topology from "../fixtures/topology.json";
import webLinks from "../fixtures/web-links.json";
import { isExplicitTerminalFilePath, resolveTerminalFilePath, terminalFileLinkCellRange, terminalFileLinks } from "./terminalFilePaths";
import { terminalLinksForBufferLine } from "./terminalLinks";
import {
  captureTerminalSelection,
  cleanWrappedCommandSelection,
  type SelectionReadableTerminal,
  type TerminalSelectionSnapshot,
} from "./terminalSelection";
import { terminalWebLinks } from "./terminalWebLinks";
import { decodeTopologyNotification } from "./topologyNotification";

// These fixtures are also run by crates/terminal-interactions. Change one here
// and the Rust port must agree, or the two implementations have drifted.

interface GridRow {
  text: string;
  wrapped: boolean;
  /** Indexes, in code points of `text`, of the glyphs that take two cells. */
  wide?: number[];
  styled?: boolean;
}

interface GridCase {
  name: string;
  columns: number;
  bufferRows: number;
  input: string;
  rows: GridRow[];
}

interface LinkGridCase extends GridCase {
  queries: { row: number; links: unknown[] }[];
}

async function terminalFor(item: GridCase): Promise<HeadlessTerminal> {
  const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: item.columns, rows: item.bufferRows, scrollback: 100 });
  await new Promise<void>((resolve) => terminal.write(item.input, resolve));
  return terminal;
}

/** The Rust tests rebuild cells from `rows`, so `rows` must be what xterm really produces. */
function expectGridMatchesXterm(terminal: HeadlessTerminal, item: GridCase) {
  const buffer = terminal.buffer.active;
  expect(buffer.length).toBe(item.bufferRows);
  const actual: GridRow[] = [];
  for (let index = 0; index < buffer.length; index += 1) {
    const line = buffer.getLine(index)!;
    const wide: number[] = [];
    const styles = new Set<boolean>();
    let codePoint = 0;
    for (let cellIndex = 0; cellIndex < line.length; cellIndex += 1) {
      const cell = line.getCell(cellIndex)!;
      if (cell.getWidth() === 0) continue;
      const chars = cell.getChars();
      expect([...chars].length, "combined cells are not modelled by the grid").toBeLessThanOrEqual(1);
      if (cell.getWidth() === 2) wide.push(codePoint);
      codePoint += 1;
      if (chars.trim() !== "") styles.add(!cell.isAttributeDefault());
    }
    expect(styles.size, "a row mixes styled and unstyled glyphs").toBeLessThanOrEqual(1);
    const row: GridRow = { text: line.translateToString(true), wrapped: line.isWrapped };
    if (wide.length) row.wide = wide;
    if (item.rows.some((expected) => expected.styled !== undefined)) row.styled = styles.has(true);
    actual.push(row);
  }
  expect(actual.slice(0, item.rows.length)).toEqual(item.rows);
  for (const blank of actual.slice(item.rows.length)) expect(blank).toMatchObject({ text: "", wrapped: false });
}

describe("shared fixtures: web links", () => {
  it.each(webLinks)("$name", ({ line, links }) => {
    expect(terminalWebLinks(line)).toEqual(links);
  });
});

describe("shared fixtures: file links", () => {
  it.each(fileLinks.links)("$name", ({ line, links }) => {
    expect(terminalFileLinks(line)).toEqual(links);
  });

  it.each(fileLinks.explicit)("explicit path: $value", ({ value, expected }) => {
    expect(isExplicitTerminalFilePath(value)).toBe(expected);
  });

  it.each(fileLinks.resolve)("resolve $value from $cwd", ({ value, cwd, home, expected }) => {
    expect(resolveTerminalFilePath(value, cwd, home)).toBe(expected ?? undefined);
  });

  it.each(fileLinks.cellRange)("cell range: $name", ({ cells, start, end, expected }) => {
    const line = {
      length: cells.length,
      getCell: (index: number) => ({ getChars: () => cells[index]?.chars ?? "", getWidth: () => cells[index]?.width ?? 1 }),
    };
    expect(terminalFileLinkCellRange(line, start, end)).toEqual(expected ?? undefined);
  });
});

describe("shared fixtures: links across rendered rows", () => {
  it.each(bufferLinks as unknown as LinkGridCase[])("$name", async (item) => {
    const { queries } = item;
    const terminal = await terminalFor(item);
    expectGridMatchesXterm(terminal, item);
    for (const query of queries) {
      expect(terminalLinksForBufferLine(terminal.buffer.active, terminal.cols, query.row), `row ${query.row}`).toEqual(query.links);
    }
    terminal.dispose();
  });
});

describe("shared fixtures: selection capture", () => {
  it.each(selectionCapture as unknown as (GridCase & {
    selection: { text: string; start?: { x: number; y: number }; end?: { x: number; y: number } };
    expected: Omit<TerminalSelectionSnapshot, "text">;
    cleaned: string;
  })[])("$name", async (item) => {
    const terminal = await terminalFor(item);
    expectGridMatchesXterm(terminal, item);
    const { text, start, end } = item.selection;
    const readable = Object.assign(terminal, {
      getSelection: () => text,
      getSelectionPosition: () => (start && end ? { start, end } : undefined),
    }) as unknown as SelectionReadableTerminal;
    const snapshot = captureTerminalSelection(readable);
    expect(snapshot).toEqual({ ...item.expected, text });
    expect(cleanWrappedCommandSelection(snapshot)).toBe(item.cleaned);
    terminal.dispose();
  });
});

describe("shared fixtures: wrapped command cleanup", () => {
  it.each(selectionClean as unknown as { name: string; snapshot: TerminalSelectionSnapshot; expected: string }[])(
    "$name",
    ({ snapshot, expected }) => {
      expect(cleanWrappedCommandSelection(snapshot)).toBe(expected);
    },
  );
});

describe("shared fixtures: topology notifications", () => {
  it.each(topology)("$label", ({ label, expected }) => {
    if (expected === null) expect(() => decodeTopologyNotification(label)).toThrow();
    else expect(decodeTopologyNotification(label)).toEqual(expected);
  });
});
