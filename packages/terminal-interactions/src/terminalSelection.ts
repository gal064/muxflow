import type { IBuffer, IBufferRange } from "@xterm/xterm";

export interface TerminalSelectionRow {
  /** Selected text on this physical buffer row, with terminal padding removed. */
  text: string;
  /** xterm's own soft-wrap marker. Hard application line breaks leave this false. */
  isWrapped: boolean;
  /** Cell immediately after the last non-whitespace glyph on the complete buffer row. */
  occupiedEndCell: number;
  /** First non-whitespace cell on the complete row, or the terminal width when blank. */
  firstTextCell: number;
  /** Display-cell width of the row's first token. */
  firstTokenCells: number;
  /** Exclusive end of the selected range on this row. */
  selectedEndCell: number;
  /** Non-whitespace selected cells and the subset carrying non-default ANSI attributes. */
  textCells: number;
  styledTextCells: number;
}

export interface TerminalSelectionSnapshot {
  /** Exactly what xterm would otherwise write to the clipboard. */
  text: string;
  columns: number;
  rows: readonly TerminalSelectionRow[];
  /** False for missing/stale rows and xterm's rectangular selection mode. */
  metadataComplete: boolean;
}

export interface SelectionReadableTerminal {
  readonly cols: number;
  readonly buffer: { readonly active: IBuffer };
  getSelection(): string;
  getSelectionPosition(): IBufferRange | undefined;
}

const normalizedSelectionText = (value: string) => value.replace(/\r\n/gu, "\n").replace(/\u00a0/gu, " ");

function rowMetrics(
  buffer: IBuffer,
  rowIndex: number,
  startCell: number,
  endCell: number,
  terminalColumns: number,
): TerminalSelectionRow | undefined {
  const line = buffer.getLine(rowIndex);
  if (!line) return undefined;
  let occupiedEndCell = 0;
  let firstTextCell = terminalColumns;
  let firstTokenCells = 0;
  let insideFirstToken = false;
  let firstTokenFinished = false;
  let textCells = 0;
  let styledTextCells = 0;

  for (let cellIndex = 0; cellIndex < terminalColumns; cellIndex += 1) {
    const cell = line.getCell(cellIndex);
    if (!cell || cell.getWidth() === 0) continue;
    const chars = cell.getChars();
    const whitespace = chars.length === 0 || /^\s+$/u.test(chars);
    const width = Math.max(1, cell.getWidth());
    if (!whitespace) {
      occupiedEndCell = cellIndex + width;
      if (firstTextCell === terminalColumns) {
        firstTextCell = cellIndex;
        insideFirstToken = true;
      }
      if (insideFirstToken && !firstTokenFinished) firstTokenCells += width;
      if (cellIndex >= startCell && cellIndex < endCell) {
        textCells += 1;
        if (!cell.isAttributeDefault()) styledTextCells += 1;
      }
    } else if (insideFirstToken) {
      insideFirstToken = false;
      firstTokenFinished = true;
    }
  }

  return {
    text: line.translateToString(true, startCell, endCell).replace(/\u00a0/gu, " "),
    isWrapped: line.isWrapped,
    occupiedEndCell,
    firstTextCell,
    firstTokenCells,
    selectedEndCell: endCell,
    textCells,
    styledTextCells,
  };
}

/**
 * Takes the text and its physical-buffer evidence in one synchronous read.
 * Clipboard I/O is asynchronous; retaining live IBufferLine objects across it
 * would let later terminal output change the evidence underneath the copy.
 */
export function captureTerminalSelection(terminal: SelectionReadableTerminal): TerminalSelectionSnapshot {
  const text = terminal.getSelection();
  const position = terminal.getSelectionPosition();
  const columns = terminal.cols;
  if (!position || !text || columns < 1) return { text, columns, rows: [], metadataComplete: false };
  if (position.start.y > position.end.y
    || (position.start.y === position.end.y && position.start.x >= position.end.x)) {
    return { text, columns, rows: [], metadataComplete: false };
  }

  const buffer = terminal.buffer.active;
  const rows: TerminalSelectionRow[] = [];
  for (let rowIndex = position.start.y; rowIndex <= position.end.y; rowIndex += 1) {
    const startCell = rowIndex === position.start.y ? position.start.x : 0;
    const endCell = rowIndex === position.end.y ? position.end.x : columns;
    const row = rowMetrics(buffer, rowIndex, startCell, endCell, columns);
    if (!row) return { text, columns, rows: [], metadataComplete: false };
    rows.push(row);
  }

  // Reproduce xterm's normal-selection join. A rectangular selection produces
  // different text for the same coordinates and therefore fails closed here.
  const logicalRows: string[] = [];
  for (const row of rows) {
    if (row.isWrapped && logicalRows.length > 0) logicalRows[logicalRows.length - 1] += row.text;
    else logicalRows.push(row.text);
  }
  const reconstructed = logicalRows.join("\n");
  return {
    text,
    columns,
    rows,
    metadataComplete: normalizedSelectionText(text) === reconstructed,
  };
}

const SHELL_OPTION = /^--?[A-Za-z0-9][A-Za-z0-9-]*$/u;
const SHELL_OPTION_ANYWHERE = /(?:^|\s)--?[A-Za-z0-9]/gu;
const COMMAND_TOKEN = /^(?:[a-z0-9_+.-]+|(?:\.{0,2}|~)?\/[a-z0-9_+./-]+)$/u;
const BARE_COMMAND_TOKEN = /^[a-z][a-z0-9_+-]*$/u;
const PLACEHOLDER_TOKEN = /^[A-Z][A-Z0-9_-]*$/u;
const HELP_TEXT = /(?:^|\s)(?:usage|options|commands):|\[(?:options?|flags?)\]/iu;
const CONTROL_KEYWORD = /(?:^|\s)(?:if|then|fi|for|while|until|case|esac|do|done|function|else|elif)(?:\s|$)/u;

function hasUnsafeShellStructure(lines: readonly string[]): boolean {
  let quote: "'" | "\"" | undefined;
  for (const line of lines) {
    if (/\\\s*$/u.test(line)) return true;
    let escaped = false;
    for (let index = 0; index < line.length; index += 1) {
      const character = line[index];
      const next = line[index + 1];
      if (escaped) {
        escaped = false;
        continue;
      }
      if (character === "\\" && quote !== "'") {
        escaped = true;
        continue;
      }
      if (quote) {
        if (character === quote) quote = undefined;
        continue;
      }
      if (character === "'" || character === "\"") {
        quote = character;
        continue;
      }
      if (character === "`" || character === ";" || character === "|" || character === "&"
        || character === "{" || character === "}" || character === "[" || character === "]"
        || character === "(" || character === ")" || character === "<" || character === ">"
        || (character === "$" && next === "(")) return true;
    }
    // A quote crossing a physical newline means that newline can be semantic.
    if (quote || escaped) return true;
  }
  return false;
}

/**
 * A "hyphen" boundary is a word the renderer split after an interior hyphen
 * (textwrap-style): "<word>-" ends the row and the rest of the word starts the
 * next one. Alphanumerics on both sides exclude bare "-" and "--" arguments.
 */
type BoundaryKind = "space" | "hyphen";
const HYPHEN_SPLIT_TAIL = /[A-Za-z0-9]-$/u;
const HYPHEN_SPLIT_HEAD = /^[A-Za-z0-9]/u;

function boundaryKind(previous: string, next: string): BoundaryKind {
  return HYPHEN_SPLIT_TAIL.test(previous) && HYPHEN_SPLIT_HEAD.test(next) ? "hyphen" : "space";
}

/** Returns one boundary kind per row break, or undefined when the text must not be joined. */
function shellShapeAllowsJoin(lines: readonly string[]): readonly BoundaryKind[] | undefined {
  const trimmed = lines.map((line) => line.trim());
  if (trimmed.some((line) => !line || /\S[ \t]{2,}\S/u.test(line))) return undefined;
  const firstLine = trimmed[0]!;
  const firstToken = firstLine.split(/\s+/u, 1)[0]!;
  if (!COMMAND_TOKEN.test(firstToken) || firstToken.startsWith("-")) return undefined;
  const joined = trimmed.join(" ");
  if (HELP_TEXT.test(joined) || CONTROL_KEYWORD.test(joined) || hasUnsafeShellStructure(trimmed)) return undefined;

  const kinds = trimmed.slice(1).map((line, index) => boundaryKind(trimmed[index]!, line));
  // A split word is its own evidence of a display wrap. Space boundaries need
  // the command to look flag-driven before any of them can be trusted.
  if (kinds.includes("space") && [...joined.matchAll(SHELL_OPTION_ANYWHERE)].length < 2) return undefined;

  for (let index = 0; index < kinds.length; index += 1) {
    if (kinds[index] === "hyphen") continue;
    const previousTokens = trimmed[index]!.split(/\s+/u);
    const nextTokens = trimmed[index + 1]!.split(/\s+/u);
    // A new indented command can follow an earlier command that happened to
    // end in a flag. Plain executable-shaped words are too ambiguous to treat
    // as that flag's value; hosts, paths and numeric values remain eligible.
    const nextToken = nextTokens[0]!;
    if (BARE_COMMAND_TOKEN.test(nextToken) || PLACEHOLDER_TOKEN.test(nextToken)) return undefined;
    if (!SHELL_OPTION.test(previousTokens.at(-1) ?? "") && !SHELL_OPTION.test(nextToken)) return undefined;
  }
  return kinds;
}

/**
 * Joins only high-confidence application-rendered command wraps. Text alone
 * can never distinguish a display break from an intentional newline, so every
 * missing or contradictory buffer signal returns the original bytes.
 */
export function cleanWrappedCommandSelection(snapshot: TerminalSelectionSnapshot): string {
  if (!snapshot.metadataComplete || snapshot.rows.length < 2 || snapshot.rows.length > 20) return snapshot.text;
  const normalized = normalizedSelectionText(snapshot.text);
  const lines = normalized.split("\n");
  if (lines.length !== snapshot.rows.length) return snapshot.text;
  const kinds = shellShapeAllowsJoin(lines);
  if (!kinds) return snapshot.text;
  if (snapshot.rows.some((row) => row.isWrapped || row.textCells === 0 || row.styledTextCells !== row.textCells)) {
    return snapshot.text;
  }
  if (snapshot.rows.some((row) => row.selectedEndCell < row.occupiedEndCell)) return snapshot.text;

  const continuationGutter = snapshot.rows[1]!.firstTextCell;
  if (continuationGutter < 1 || continuationGutter > 8
    || snapshot.rows.slice(1).some((row) => row.firstTextCell !== continuationGutter)) return snapshot.text;

  for (let index = 0; index < snapshot.rows.length - 1; index += 1) {
    const row = snapshot.rows[index]!;
    const next = snapshot.rows[index + 1]!;
    if (next.firstTokenCells < 1) return snapshot.text;
    // The renderer greedily broke before next's first token: that token, plus
    // one separating space unless it continues a hyphen-split word, did not
    // fit in the cells remaining on this row.
    const separatorCells = kinds[index] === "hyphen" ? 0 : 1;
    if (row.occupiedEndCell + separatorCells + next.firstTokenCells < snapshot.columns) return snapshot.text;
  }

  return lines.reduce((joined, line, index) => (
    index === 0 ? line.trim() : `${joined}${kinds[index - 1] === "hyphen" ? "" : " "}${line.trim()}`
  ), "");
}
