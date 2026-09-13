import { Terminal as HeadlessTerminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import {
  captureTerminalSelection,
  cleanWrappedCommandSelection,
  type SelectionReadableTerminal,
  type TerminalSelectionRow,
  type TerminalSelectionSnapshot,
} from "./terminalSelection";

const SSH_LINES = [
  "ssh -N -L 18080:127.0.0.1:18080 -L 8100:127.0.0.1:8100 -L 8200:127.0.0.1:8200",
  "  -L 4443:127.0.0.1:4443 -L 19001:127.0.0.1:19001 -L 5434:127.0.0.1:5434 -L",
  "  55433:127.0.0.1:55433 -L 56379:127.0.0.1:56379 omarchy",
] as const;

const CLEAN_SSH = SSH_LINES.map((line) => line.trim()).join(" ");

// A flagless command whose last token the renderer split after an interior hyphen.
const SCP_LINES = [
  "scp dev@100.112.254.120:/home/user/dev/muxflow-mobile/apps/mobile/android/app/build/outputs/apk/debug/app-debug.apk ~/Downloads/muxflow-",
  "  debug.apk",
] as const;

const CLEAN_SCP = SCP_LINES[0] + SCP_LINES[1].trim();
// Wide enough for the first row, too narrow for "debug.apk" to have continued it.
const SCP_COLUMNS = SCP_LINES[0].length + 5;

function firstTokenCells(value: string): number {
  return value.trimStart().split(/\s+/u, 1)[0].length;
}

function row(text: string, overrides: Partial<TerminalSelectionRow> = {}): TerminalSelectionRow {
  const indent = text.length - text.trimStart().length;
  const textCells = [...text].filter((character) => !/\s/u.test(character)).length;
  return {
    text,
    isWrapped: false,
    occupiedEndCell: text.length,
    firstTextCell: indent,
    firstTokenCells: firstTokenCells(text),
    selectedEndCell: 80,
    textCells,
    styledTextCells: textCells,
    ...overrides,
  };
}

function snapshot(
  lines: readonly string[] = SSH_LINES,
  overrides: Partial<TerminalSelectionSnapshot> = {},
): TerminalSelectionSnapshot {
  return {
    text: lines.join("\n"),
    columns: 80,
    metadataComplete: true,
    rows: lines.map((line) => row(line)),
    ...overrides,
  };
}

describe("wrapped command cleanup", () => {
  it("joins the reported SSH command only after every rendered boundary proves it wrapped", () => {
    expect(cleanWrappedCommandSelection(snapshot())).toBe(CLEAN_SSH);
  });

  it("accepts xterm's CRLF clipboard text and tab-shaped continuation evidence", () => {
    expect(cleanWrappedCommandSelection(snapshot(SSH_LINES, { text: SSH_LINES.join("\r\n") }))).toBe(CLEAN_SSH);
    const tabbed = [SSH_LINES[0], `\t${SSH_LINES[1].trim()}`, `\t${SSH_LINES[2].trim()}`];
    expect(cleanWrappedCommandSelection(snapshot(tabbed, {
      rows: tabbed.map((line, index) => row(line, { firstTextCell: index === 0 ? 0 : 4 })),
    }))).toBe(CLEAN_SSH);
  });

  it("recognizes the live Codex fenced-block geometry with heterogeneous syntax colors", () => {
    const lines = [
      "  ssh -N -L 18080:127.0.0.1:18080 -L 8100:127.0.0.1:8100 -L 8200:127.0.0.1:8200 -L 4443:127.0.0.1:4443 -L 19001:127.0.0.1:19001 -L",
      "  5434:127.0.0.1:5434 -L 55433:127.0.0.1:55433 -L 56379:127.0.0.1:56379 omarchy",
    ];
    expect(cleanWrappedCommandSelection(snapshot(lines, {
      columns: 145,
      rows: lines.map((line) => row(line, { selectedEndCell: 145 })),
    }))).toBe(lines.map((line) => line.trim()).join(" "));
  });

  it("rejoins a flagless command split after an interior hyphen without inserting a space", () => {
    expect(cleanWrappedCommandSelection(snapshot(SCP_LINES, {
      columns: SCP_COLUMNS,
      rows: SCP_LINES.map((line) => row(line, { selectedEndCell: SCP_COLUMNS })),
    }))).toBe(CLEAN_SCP);
  });

  it("rejoins a long flag split after one of its hyphens", () => {
    const lines = ["command --first value --dry-", "  run --second value"];
    const columns = lines[0].length + 2;
    expect(cleanWrappedCommandSelection(snapshot(lines, {
      columns,
      rows: lines.map((line) => row(line, { selectedEndCell: columns })),
    }))).toBe("command --first value --dry-run --second value");
  });

  it("keeps a hyphen-ending row when the rest of the word would have fit on it", () => {
    const columns = SCP_LINES[0].length + SCP_LINES[1].trim().length + 1;
    const candidate = snapshot(SCP_LINES, {
      columns,
      rows: SCP_LINES.map((line) => row(line, { selectedEndCell: columns })),
    });
    expect(cleanWrappedCommandSelection(candidate)).toBe(candidate.text);
  });

  it.each([
    ["missing metadata", { metadataComplete: false }],
    ["native xterm soft wrap", { rows: SSH_LINES.map((line, index) => row(line, { isWrapped: index === 1 })) }],
    ["unstyled terminal prose", { rows: SSH_LINES.map((line) => row(line, { styledTextCells: 0 })) }],
    ["partial selection", { rows: SSH_LINES.map((line, index) => row(line, { selectedEndCell: index === 0 ? 40 : 80 })) }],
    ["partial final row", { rows: SSH_LINES.map((line, index) => row(line, index === 2 ? { occupiedEndCell: 70, selectedEndCell: 56 } : {})) }],
    ["stale wide geometry", { columns: 120, rows: SSH_LINES.map((line) => row(line, { selectedEndCell: 120 })) }],
    ["inconsistent gutters", { rows: SSH_LINES.map((line, index) => row(line, { firstTextCell: index })) }],
  ])("preserves the exact bytes for %s", (_label, overrides) => {
    const candidate = snapshot(SSH_LINES, overrides as Partial<TerminalSelectionSnapshot>);
    expect(cleanWrappedCommandSelection(candidate)).toBe(candidate.text);
  });

  it.each([
    ["explicit continuation", ["ssh -N -L first \\", "  -L second --flag value"]],
    ["pipeline", ["curl --silent --fail https://example.test |", "  jq --raw-output .value"]],
    ["logical commands", ["command --first value &&", "  other --second value"]],
    ["heredoc", ["command --first value <<EOF", "  --second value"]],
    ["control flow", ["for value --first item", "  do command --second item"]],
    ["multiline quote", ["printf --format '%s --first", "  value --second'"]],
    ["command substitution", ["command --first $(other", "  --second value)"]],
    ["usage text", ["ssh [options] --first value --long filler", "  --second  Description of option"]],
    ["angle-bracket synopsis", ["ssh --verbose --identity-file", "  <key-path> <host>"]],
    ["uppercase synopsis", ["ssh --verbose --identity-file", "  KEY_PATH HOST"]],
    ["prose", ["Use ssh -N --first value --long filler", "  --second means something else"]],
    ["two commands", ["ssh --first value --long filler;", "  curl --second value"]],
    ["two commands without a separator", ["git status --short --branch", "  git log --oneline"]],
    ["flagless command wrapped at a space", ["scp dev@host:/path/app-debug.apk ~/Downloads/", "  muxflow-debug.apk"]],
    ["lone hyphen argument", ["cat -", "  file.txt"]],
    ["hyphen split followed by a flagless space boundary", ["scp dev@host:/a/b/app-", "  debug.apk ~/Downloads/", "  out.apk"]],
    ["hyphen-ending quote", ["printf --format 'foo-", "  bar' --second value"]],
    ["blank line", [SSH_LINES[0], "  ", SSH_LINES[2]]],
  ])("does not rewrite intentional or non-command multiline %s", (_label, lines) => {
    const candidate = snapshot(lines);
    expect(cleanWrappedCommandSelection(candidate)).toBe(candidate.text);
  });

  it("is idempotent and preserves the ordered non-whitespace character sequence", () => {
    const cleaned = cleanWrappedCommandSelection(snapshot());
    expect(cleanWrappedCommandSelection(snapshot([cleaned]))).toBe(cleaned);
    expect(cleaned.replace(/\s/gu, "")).toBe(SSH_LINES.join("").replace(/\s/gu, ""));
  });

  it("fails closed around generated geometry, style, and syntax hazards", () => {
    const hazards = [";", "|", "&&", "`", "$(", "<<"];
    for (let columns = 81; columns <= 110; columns += 1) {
      const candidate = snapshot(SSH_LINES, {
        columns,
        rows: SSH_LINES.map((line) => row(line, { selectedEndCell: columns })),
      });
      expect(cleanWrappedCommandSelection(candidate), `columns=${columns}`).toBe(candidate.text);
    }
    for (const hazard of hazards) {
      const lines = [SSH_LINES[0] + hazard, SSH_LINES[1], SSH_LINES[2]];
      expect(cleanWrappedCommandSelection(snapshot(lines)), hazard).toBe(lines.join("\n"));
    }
  });
});

describe("xterm selection evidence", () => {
  const write = (terminal: HeadlessTerminal, value: string) => new Promise<void>((resolve) => terminal.write(value, resolve));

  it("captures a sanitized ANSI fixture synchronously and cleans the resulting hard rows", async () => {
    const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: 80, rows: 5 });
    await write(terminal, `\x1b[38;5;6m${SSH_LINES[0]}\x1b[39m\r\n  \x1b[38;5;6m${SSH_LINES[1].trim()}\x1b[39m\r\n  \x1b[38;5;6m${SSH_LINES[2].trim()}\x1b[39m`);
    const readable = Object.assign(terminal, {
      getSelection: () => SSH_LINES.join("\n"),
      getSelectionPosition: () => ({ start: { x: 0, y: 0 }, end: { x: SSH_LINES[2].length, y: 2 } }),
    }) as unknown as SelectionReadableTerminal;

    const captured = captureTerminalSelection(readable);
    expect(captured).toMatchObject({ columns: 80, metadataComplete: true });
    expect(captured.rows.map(({ isWrapped, firstTextCell }) => ({ isWrapped, firstTextCell }))).toEqual([
      { isWrapped: false, firstTextCell: 0 },
      { isWrapped: false, firstTextCell: 2 },
      { isWrapped: false, firstTextCell: 2 },
    ]);
    expect(cleanWrappedCommandSelection(captured)).toBe(CLEAN_SSH);
    terminal.dispose();
  });

  it("measures a hyphen-split word from the buffer and rejoins it without a space", async () => {
    const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: SCP_COLUMNS, rows: 3 });
    await write(terminal, `\x1b[36m${SCP_LINES[0]}\x1b[0m\r\n  \x1b[36m${SCP_LINES[1].trim()}\x1b[0m`);
    const readable = Object.assign(terminal, {
      getSelection: () => SCP_LINES.join("\n"),
      getSelectionPosition: () => ({ start: { x: 0, y: 0 }, end: { x: SCP_LINES[1].length, y: 1 } }),
    }) as unknown as SelectionReadableTerminal;

    const captured = captureTerminalSelection(readable);
    expect(captured.metadataComplete).toBe(true);
    expect(captured.rows.map(({ isWrapped, firstTokenCells }) => ({ isWrapped, firstTokenCells }))).toEqual([
      { isWrapped: false, firstTokenCells: 3 },
      { isWrapped: false, firstTokenCells: SCP_LINES[1].trim().length },
    ]);
    expect(cleanWrappedCommandSelection(captured)).toBe(CLEAN_SCP);
    terminal.dispose();
  });

  it("proves native xterm wraps are already represented as soft rows and remain untouched", async () => {
    const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: 12, rows: 4 });
    await write(terminal, "\x1b[36mcommand --first value --second value\x1b[0m");
    const readable = Object.assign(terminal, {
      getSelection: () => "command --first value --second value",
      getSelectionPosition: () => ({ start: { x: 0, y: 0 }, end: { x: 8, y: 2 } }),
    }) as unknown as SelectionReadableTerminal;
    const captured = captureTerminalSelection(readable);
    expect(captured.rows.slice(1).every((item) => item.isWrapped)).toBe(true);
    expect(cleanWrappedCommandSelection(captured)).toBe(captured.text);
    terminal.dispose();
  });

  it("sees unselected trailing cells on the final physical row and fails closed", async () => {
    const first = "ssh -N --first value --second";
    const selectedFinal = "  destination.example";
    const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: 46, rows: 3 });
    await write(terminal, `\x1b[36m${first}\x1b[0m\r\n\x1b[36m${selectedFinal} trailing-text\x1b[0m`);
    const readable = Object.assign(terminal, {
      getSelection: () => `${first}\n${selectedFinal}`,
      getSelectionPosition: () => ({ start: { x: 0, y: 0 }, end: { x: selectedFinal.length, y: 1 } }),
    }) as unknown as SelectionReadableTerminal;

    const captured = captureTerminalSelection(readable);
    expect(captured.metadataComplete).toBe(true);
    expect(captured.rows[1].occupiedEndCell).toBeGreaterThan(captured.rows[1].selectedEndCell);
    expect(cleanWrappedCommandSelection(captured)).toBe(captured.text);
    terminal.dispose();
  });

  it("uses terminal cell widths for CJK and emoji instead of JavaScript string length", async () => {
    const lines = ["curl --silent --header 'X-City: 東京' --url", "  https://例え.test/🚀"];
    const terminal = new HeadlessTerminal({ allowProposedApi: true, cols: 60, rows: 3 });
    await write(terminal, `\x1b[36m${lines[0]}\x1b[0m\r\n  \x1b[36m${lines[1].trim()}\x1b[0m`);
    const readable = Object.assign(terminal, {
      getSelection: () => lines.join("\n"),
      getSelectionPosition: () => ({ start: { x: 0, y: 0 }, end: { x: 60, y: 1 } }),
    }) as unknown as SelectionReadableTerminal;

    const captured = captureTerminalSelection(readable);
    expect(captured.rows[1].firstTokenCells).toBeGreaterThan(lines[1].trim().length);
    expect(cleanWrappedCommandSelection(captured)).toBe(lines.map((line) => line.trim()).join(" "));
    terminal.dispose();
  });
});
