import type { IBuffer, IBufferLine, IBufferRange } from "@xterm/xterm";
import { terminalFileLinks } from "./terminalFilePaths";
import { terminalWebLinks } from "./terminalWebLinks";

export interface TerminalDetectedLink {
  kind: "file" | "web";
  text: string;
  range: IBufferRange;
}

interface LogicalRow {
  line: IBufferLine;
  row: number;
  textStart: number;
  textEnd: number;
  columns: number;
}

interface HardWrappedLinkCandidate {
  kind: TerminalDetectedLink["kind"] | "delimited";
  rawText: string;
  range: IBufferRange;
  lastRow: number;
}

interface LogicalLine {
  text: string;
  rows: LogicalRow[];
  lastRow: number;
}

/**
 * Bounds work done from a mouse hover over pathological output containing one
 * enormous soft-wrapped line. Ordinary filesystem paths fit comfortably.
 */
const MAX_LOGICAL_ROWS = 128;

/** Bounds application-rendered logical lines and their physical reflow. */
const MAX_HARD_LINK_LOGICAL_LINES = 20;
const MAX_HARD_LINK_PHYSICAL_ROWS = MAX_LOGICAL_ROWS;

/**
 * A rendered list item marker, at any indentation. Agent TUIs indent nested
 * items, so the marker is evidence of a rendered list even when it does not sit
 * in the first column.
 */
const LIST_ITEM_MARKER = /^\s*(?:[•●*+-]|\d+(?:\.\d+)*[.)])\s/u;

/** URL punctuation after which a terminal application may paint the rest on another hard row. */
const WEB_HARD_WRAP_BOUNDARIES = new Set([
  "-", ".", "_", "~", ":", "/", "?", "#", "[", "@",
  "!", "$", "&", "(", "*", "+", ",", ";", "=", "%",
]);

/**
 * Finds links in the complete logical line containing one physical buffer row.
 *
 * xterm calls a link provider with a rendered row number. A long token can
 * cross that row's soft-wrap boundary, so parsing that row alone produces only
 * the prefix or suffix. `isWrapped` is authoritative evidence that no newline
 * occurred; only those rows are joined here.
 */
export function terminalLinksForBufferLine(
  buffer: Pick<IBuffer, "getLine" | "length">,
  columns: number,
  bufferLineNumber: number,
): TerminalDetectedLink[] {
  if (columns <= 0 || bufferLineNumber < 1 || bufferLineNumber > buffer.length) return [];
  const requestedRow = bufferLineNumber - 1;
  const logicalLine = logicalLineContaining(buffer, columns, requestedRow, MAX_LOGICAL_ROWS);
  if (!logicalLine) return [];
  const { rows, text } = logicalLine;

  const detected = [
    ...terminalWebLinks(text).map((link) => ({ ...link, kind: "web" as const })),
    ...terminalFileLinks(text).map((link) => ({ ...link, kind: "file" as const })),
  ];
  const links: TerminalDetectedLink[] = [];
  for (const link of detected) {
    const range = bufferRange(rows, link.start, link.end);
    // A provider is asked about the hovered row. Returning links elsewhere in
    // the same logical line needlessly gives xterm ranges it cannot activate
    // at this pointer position.
    if (!range || bufferLineNumber < range.start.y || bufferLineNumber > range.end.y) continue;
    links.push({ kind: link.kind, text: link.text, range });
  }

  const hardWrappedLinks = applicationHardWrappedLinks(buffer, columns, requestedRow);
  if (!hardWrappedLinks.length) return links;
  return [
    ...links.filter((link) => !hardWrappedLinks.some((complete) => rangeContains(complete.range, link.range))),
    ...hardWrappedLinks,
  ].sort((left, right) => comparePosition(left.range.start, right.range.start));
}

/**
 * Full-screen applications may paint visually wrapped prose as separate hard
 * terminal rows. Reconstruct links only from bounded, kind-specific evidence:
 * known link prefixes, an open target delimiter, and indented continuations.
 * This deliberately does not guess from terminal width because applications
 * can wrap inside an inset content region.
 */
function applicationHardWrappedLinks(
  buffer: Pick<IBuffer, "getLine" | "length">,
  columns: number,
  requestedRow: number,
): TerminalDetectedLink[] {
  const links: TerminalDetectedLink[] = [];
  for (const origin of hardWrappedCandidateOrigins(buffer, requestedRow)) {
    const logicalLine = logicalLineStartingAt(buffer, columns, origin, MAX_HARD_LINK_PHYSICAL_ROWS);
    if (!logicalLine) continue;
    const candidate = hardWrappedLinkCandidate(logicalLine);
    if (!candidate) continue;
    const extended = extendApplicationHardWrappedLink(buffer, columns, origin, candidate);
    if (extended
      && rangeContainsRow(extended.range, requestedRow + 1)
      && !links.some((link) => rangeContains(link.range, extended.range))) links.push(extended);
  }
  return links;
}

function hardWrappedCandidateOrigins(
  buffer: Pick<IBuffer, "getLine">,
  requestedRow: number,
): number[] {
  const firstPhysicalRow = Math.max(0, requestedRow - MAX_HARD_LINK_PHYSICAL_ROWS + 1);
  const origins: number[] = [];
  let row = requestedRow;
  while (row >= firstPhysicalRow && origins.length < MAX_HARD_LINK_LOGICAL_LINES) {
    while (row > firstPhysicalRow && buffer.getLine(row)?.isWrapped) row -= 1;
    const line = buffer.getLine(row);
    if (!line || line.isWrapped) break;
    origins.push(row);
    row -= 1;
  }
  return origins.reverse();
}

function hardWrappedLinkCandidate(logicalLine: LogicalLine): HardWrappedLinkCandidate | undefined {
  const lineText = logicalLine.text;
  const isListItem = LIST_ITEM_MARKER.test(lineText);
  const opening = unclosedTargetOpening(lineText);
  const file = terminalFileLinks(lineText).find((candidate) => (
    candidate.end === lineText.length
      && canContinueHardWrappedFileLink(candidate.text)
      && candidate.start !== (opening === undefined ? -1 : opening + 1)
  ));
  if (file) {
    const range = bufferRange(logicalLine.rows, file.start, file.end);
    if (range) {
      return {
        kind: "file",
        rawText: file.text,
        range,
        lastRow: logicalLine.lastRow,
      };
    }
  }

  const web = terminalWebLinks(lineText).reverse().find((candidate) => {
    const rawText = lineText.slice(candidate.start);
    const trimmedSuffix = rawText.slice(candidate.text.length);
    return !/\s/u.test(rawText)
      && !/[\]\)}]/u.test(trimmedSuffix)
      && canContinueHardWrappedWebLink(rawText);
  });
  if (web) {
    const rawText = lineText.slice(web.start);
    const range = bufferRange(logicalLine.rows, web.start, lineText.length);
    if (!range) return undefined;
    return {
      kind: "web",
      rawText,
      range,
      lastRow: logicalLine.lastRow,
    };
  }

  // A rendered Markdown-style target can wrap before its first slash, so its
  // first fragment is not yet a file path. The unmatched opening parenthesis
  // and eventual closing parenthesis provide the missing boundary evidence.
  if (opening !== undefined) {
    const rawText = lineText.slice(opening + 1);
    const range = bufferRange(logicalLine.rows, opening + 1, lineText.length);
    if (!range) return undefined;
    return {
      kind: "delimited",
      rawText,
      range,
      lastRow: logicalLine.lastRow,
    };
  }

  // A plain path in a rendered list item can also wrap before its first slash.
  // In that case the first row is not a path yet, but a trailing '-' or '/' is
  // explicit continuation evidence. Accept it as a candidate only; the joined
  // text must still parse as a file path before any link is returned.
  const fragment = isListItem ? /(\S+[-/])$/u.exec(lineText) : undefined;
  if (!fragment) return undefined;
  const rawText = fragment[1]!;
  const start = lineText.length - rawText.length;
  const range = bufferRange(logicalLine.rows, start, lineText.length);
  if (!range) return undefined;
  return {
    kind: "file",
    rawText,
    range,
    lastRow: logicalLine.lastRow,
  };
}

/** Reads the complete xterm logical line containing one physical row. */
function logicalLineContaining(
  buffer: Pick<IBuffer, "getLine" | "length">,
  columns: number,
  requestedRow: number,
  maxRows: number,
): LogicalLine | undefined {
  let firstRow = requestedRow;
  while (firstRow > 0 && buffer.getLine(firstRow)?.isWrapped) {
    firstRow -= 1;
    if (requestedRow - firstRow + 1 > maxRows) return undefined;
  }
  return logicalLineStartingAt(buffer, columns, firstRow, maxRows);
}

/** Reads one xterm logical line beginning at a non-wrapped physical row. */
function logicalLineStartingAt(
  buffer: Pick<IBuffer, "getLine" | "length">,
  columns: number,
  firstRow: number,
  maxRows: number,
): LogicalLine | undefined {
  const firstLine = buffer.getLine(firstRow);
  if (!firstLine || firstLine.isWrapped || maxRows < 1) return undefined;

  let lastRow = firstRow;
  while (lastRow + 1 < buffer.length && buffer.getLine(lastRow + 1)?.isWrapped) {
    lastRow += 1;
    if (lastRow - firstRow + 1 > maxRows) return undefined;
  }

  const rows: LogicalRow[] = [];
  let text = "";
  for (let row = firstRow; row <= lastRow; row += 1) {
    const line = buffer.getLine(row);
    if (!line) return undefined;
    const rowColumns = logicalRowColumns(buffer, line, row, lastRow, columns);
    const rowText = line.translateToString(row === lastRow, 0, rowColumns);
    rows.push({
      line,
      row,
      textStart: text.length,
      textEnd: text.length + rowText.length,
      columns: rowColumns,
    });
    text += rowText;
  }
  return { text, rows, lastRow };
}

/** Finds the outermost unmatched parenthesis whose suffix is one token. */
function unclosedTargetOpening(text: string): number | undefined {
  const openings: number[] = [];
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] === "(") openings.push(index);
    else if (text[index] === ")") openings.pop();
  }
  return openings.find((opening) => {
    const suffix = text.slice(opening + 1);
    return suffix.length > 0 && !/\s/u.test(suffix);
  });
}

/** Finds the closing parenthesis paired with the wrapper before `text`. */
function matchingTargetClosing(text: string): number {
  let depth = 1;
  for (let index = 0; index < text.length; index += 1) {
    if (text[index] === "(") depth += 1;
    else if (text[index] === ")") {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

function extendApplicationHardWrappedLink(
  buffer: Pick<IBuffer, "getLine" | "length">,
  columns: number,
  origin: number,
  candidate: HardWrappedLinkCandidate,
): TerminalDetectedLink | undefined {
  let rawText = candidate.rawText;
  let range = candidate.range;
  let extended: TerminalDetectedLink | undefined;
  let row = candidate.lastRow + 1;
  let logicalLines = 1;

  while (row < buffer.length && logicalLines < MAX_HARD_LINK_LOGICAL_LINES) {
    if (!canContinueApplicationHardWrappedLink(candidate, rawText)) break;
    const remainingPhysicalRows = MAX_HARD_LINK_PHYSICAL_ROWS - (row - origin);
    if (remainingPhysicalRows < 1) return undefined;
    const logicalLine = logicalLineStartingAt(
      buffer,
      columns,
      row,
      remainingPhysicalRows,
    );
    if (!logicalLine) return undefined;
    logicalLines += 1;
    const lineText = logicalLine.text;
    const match = hardWrappedContinuation(lineText);
    if (!match) break;
    if (candidate.kind === "file" && !continuesWrappedFilePath(rawText, match.text)) break;

    const combined = rawText + match.text;
    if (candidate.kind === "delimited") {
      const closing = matchingTargetClosing(combined);
      if (closing < 0) {
        if (match.end !== lineText.length) break;
        rawText = combined;
        row = logicalLine.lastRow + 1;
        continue;
      }
      const target = combined.slice(0, closing);
      const joined = parseDelimitedTarget(target);
      if (!joined || joined.text.length <= rawText.length) break;
      const appendedLength = joined.text.length - rawText.length;
      const appendedRange = bufferRange(
        logicalLine.rows,
        match.start,
        match.start + appendedLength,
      );
      if (!appendedRange) break;
      range = { start: candidate.range.start, end: appendedRange.end };
      return { kind: joined.kind, text: joined.text, range };
    }

    const joined = parseHardWrappedLink(candidate.kind, combined);
    if (!joined || joined.start !== 0 || joined.text.length <= rawText.length) break;
    const appendedLength = joined.text.length - rawText.length;
    const appendedRange = bufferRange(
      logicalLine.rows,
      match.start,
      match.start + appendedLength,
    );
    if (!appendedRange) break;

    range = { start: candidate.range.start, end: appendedRange.end };
    extended = { kind: candidate.kind, text: joined.text, range };
    if (match.end !== lineText.length) break;
    if (joined.text.length !== combined.length
      && (candidate.kind !== "web" || !canContinueHardWrappedWebLink(combined))) break;
    rawText = combined;
    row = logicalLine.lastRow + 1;
  }

  if (logicalLines >= MAX_HARD_LINK_LOGICAL_LINES
    && row < buffer.length
    && canContinueApplicationHardWrappedLink(candidate, rawText)) return undefined;
  return extended;
}

function canContinueApplicationHardWrappedLink(candidate: HardWrappedLinkCandidate, text: string): boolean {
  if (candidate.kind === "delimited") return matchingTargetClosing(text) < 0;
  return candidate.kind === "file"
    ? canContinueHardWrappedFileLink(text)
    : canContinueHardWrappedWebLink(text);
}

function canContinueHardWrappedFileLink(text: string): boolean {
  return text.endsWith("-") || text.endsWith("/");
}

function canContinueHardWrappedWebLink(text: string): boolean {
  const last = text.at(-1);
  return last !== undefined && WEB_HARD_WRAP_BOUNDARIES.has(last);
}

function hardWrappedContinuation(lineText: string): { text: string; start: number; end: number } | undefined {
  const match = /^ +(\S+)/u.exec(lineText);
  if (!match) return undefined;
  // A row that opens its own list item is the next item, not the tail of the one
  // above it. Without this, siblings splice into each other: an item ending in
  // '/' or '-' would swallow the next item's marker and link a path that was
  // never on screen.
  if (LIST_ITEM_MARKER.test(lineText)) return undefined;
  const text = match[1]!;
  const start = match[0].length - text.length;
  return { text, start, end: start + text.length };
}

/**
 * Decides whether the fragment on the next row continues a wrapped path.
 *
 * The two boundaries this joiner accepts are not equally strong evidence. A row
 * broken after '-' was split inside a token, because prose does not end a row
 * mid-word — so whatever follows continues that token. A row broken after '/'
 * is also exactly where an ordinary sentence wraps at a space, so there the
 * fragment must still look like part of a path: a separator or an extension.
 * Otherwise it is simply the next word, and joining it invents a path that was
 * never on screen.
 */
function continuesWrappedFilePath(text: string, continuation: string): boolean {
  if (text.endsWith("-")) return true;
  return continuation.includes("/") || continuation.includes(".");
}

function parseHardWrappedLink(kind: TerminalDetectedLink["kind"], text: string) {
  return kind === "file" ? terminalFileLinks(text)[0] : terminalWebLinks(text)[0];
}

function parseDelimitedTarget(text: string): Pick<TerminalDetectedLink, "kind" | "text"> | undefined {
  const web = terminalWebLinks(text)[0];
  if (web?.start === 0) return { kind: "web", text: web.text };
  const file = terminalFileLinks(text)[0];
  if (file?.start === 0) return { kind: "file", text: file.text };
  return undefined;
}

function rangeContainsRow(range: IBufferRange, row: number): boolean {
  return row >= range.start.y && row <= range.end.y;
}

function rangeContains(outer: IBufferRange, inner: IBufferRange): boolean {
  return comparePosition(outer.start, inner.start) <= 0 && comparePosition(outer.end, inner.end) >= 0;
}

function comparePosition(left: { x: number; y: number }, right: { x: number; y: number }): number {
  return left.y === right.y ? left.x - right.x : left.y - right.y;
}

/**
 * A wide glyph cannot start in the final cell. xterm leaves that cell empty
 * and wraps the glyph onto the next row; the empty cell is layout, not a space
 * in the logical text.
 */
function logicalRowColumns(
  buffer: Pick<IBuffer, "getLine">,
  line: IBufferLine,
  row: number,
  lastRow: number,
  columns: number,
): number {
  if (row === lastRow) return columns;
  const lastCell = line.getCell(columns - 1);
  const nextCell = buffer.getLine(row + 1)?.getCell(0);
  if (lastCell?.getChars() === "" && lastCell.getWidth() === 1 && nextCell?.getWidth() === 2) {
    return columns - 1;
  }
  return columns;
}

function bufferRange(rows: LogicalRow[], start: number, end: number): IBufferRange | undefined {
  const startRow = rows.find((row) => start >= row.textStart && start < row.textEnd);
  const endRow = rows.find((row) => end > row.textStart && end <= row.textEnd);
  if (!startRow || !endRow) return undefined;
  const startCell = cellAtStringStart(startRow, start - startRow.textStart);
  const endCell = cellAtStringEnd(endRow, end - endRow.textStart);
  if (startCell === undefined || endCell === undefined) return undefined;
  return {
    start: { x: startCell + 1, y: startRow.row + 1 },
    end: { x: endCell + 1, y: endRow.row + 1 },
  };
}

function cellAtStringStart(row: LogicalRow, target: number): number | undefined {
  let offset = 0;
  for (let cellIndex = 0; cellIndex < row.columns; cellIndex += 1) {
    const cell = row.line.getCell(cellIndex);
    if (!cell || cell.getWidth() === 0) continue;
    if (offset === target) return cellIndex;
    offset += cell.getChars().length || 1;
    if (offset > target) return undefined;
  }
  return undefined;
}

function cellAtStringEnd(row: LogicalRow, target: number): number | undefined {
  let offset = 0;
  for (let cellIndex = 0; cellIndex < row.columns; cellIndex += 1) {
    const cell = row.line.getCell(cellIndex);
    if (!cell || cell.getWidth() === 0) continue;
    offset += cell.getChars().length || 1;
    if (offset === target) return cellIndex + cell.getWidth() - 1;
    if (offset > target) return undefined;
  }
  return undefined;
}
