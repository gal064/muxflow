export interface TerminalFileLink {
  text: string;
  /** Zero-based UTF-16 offsets into the rendered logical line. */
  start: number;
  end: number;
}

interface TerminalBufferCell {
  getChars(): string;
  getWidth(): number;
}

interface TerminalBufferLine {
  readonly length: number;
  getCell(index: number): TerminalBufferCell | undefined;
}

const LEADING_WRAPPERS = new Set(["(", "[", "{", "'", '"', "`"]);
const TRAILING_WRAPPERS = new Set([")", "]", "}", "'", '"', "`", ",", ";", ":", "!", "?", "."]);
const CLOSING_WRAPPERS = new Map([
  [")", "("],
  ["]", "["],
  ["}", "{"],
]);

/**
 * Finds only the deliberately small P0 path vocabulary in one terminal line.
 *
 * Tokens are whitespace-delimited. Absolute paths and relative paths with a
 * slash qualify; a trailing line/column location is left outside the link.
 * URLs and bare filenames do not qualify.
 */
export function terminalFileLinks(line: string): TerminalFileLink[] {
  const links: TerminalFileLink[] = [];
  for (const match of line.matchAll(/[^\s"']+/gu)) {
    const token = match[0];
    let leading = 0;
    let trailing = token.length;
    while (leading < trailing && LEADING_WRAPPERS.has(token.charAt(leading))) leading += 1;
    while (trailing > leading && TRAILING_WRAPPERS.has(token.charAt(trailing - 1))) {
      const closing = token.charAt(trailing - 1);
      const opening = CLOSING_WRAPPERS.get(closing);
      if (opening
        && delimiterCount(token, opening, leading, trailing) >= delimiterCount(token, closing, leading, trailing)) break;
      trailing -= 1;
    }
    const wrappedText = token.slice(leading, trailing);
    const location = /:\d+(?::\d+)?$/u.exec(wrappedText);
    const text = location ? wrappedText.slice(0, location.index) : wrappedText;
    if (!isExplicitTerminalFilePath(text)) continue;
    const start = (match.index ?? 0) + leading;
    links.push({ text, start, end: start + text.length });
  }
  return links;
}

function delimiterCount(value: string, delimiter: string, start: number, end: number): number {
  let count = 0;
  for (let index = start; index < end; index += 1) {
    if (value[index] === delimiter) count += 1;
  }
  return count;
}

export function isExplicitTerminalFilePath(value: string): boolean {
  if (!value || value.includes("\0") || /:\d+(?::\d+)?$/u.test(value)) return false;
  if (value.startsWith("//")) return false;
  if (value.startsWith("/")) return true;
  if (value.startsWith("~")) {
    return value.startsWith("~/") && value.length > 2 && value[2] !== "/";
  }
  const separator = value.indexOf("/");
  if (separator < 0) return false;
  const firstComponent = value.slice(0, separator);
  if (firstComponent.includes(":")
    || /^www\./iu.test(firstComponent)
    || /^[a-z\d-]+(?:\.[a-z\d-]+)+$/iu.test(firstComponent)) return false;
  return true;
}

/** Maps a UTF-16 parser range onto xterm's zero-based, inclusive cell range. */
export function terminalFileLinkCellRange(
  line: TerminalBufferLine,
  start: number,
  end: number,
): { start: number; end: number } | undefined {
  let stringOffset = 0;
  let startCell: number | undefined;
  for (let cellIndex = 0; cellIndex < line.length; cellIndex += 1) {
    const cell = line.getCell(cellIndex);
    if (!cell || cell.getWidth() === 0) continue;
    if (stringOffset === start) startCell = cellIndex;
    const nextOffset = stringOffset + (cell.getChars().length || 1);
    if (nextOffset === end && startCell !== undefined) {
      return { start: startCell, end: cellIndex + cell.getWidth() - 1 };
    }
    stringOffset = nextOffset;
    if (stringOffset > end) return undefined;
  }
  return undefined;
}

/** POSIX lexical resolution used by tests and renderer-side diagnostics. */
export function resolveTerminalFilePath(value: string, cwd: string, home?: string): string | undefined {
  if (!isExplicitTerminalFilePath(value)) return undefined;
  if (value.startsWith("~/")) {
    if (!home?.startsWith("/") || home.includes("\0")) return undefined;
    return resolvePosixPath(`${home.replace(/\/+$/u, "")}/${value.slice(2)}`, "/");
  }
  return resolvePosixPath(value, cwd);
}

/** Lexically resolves one already-qualified terminal path. */
function resolvePosixPath(value: string, cwd: string): string | undefined {
  if (!value || value.includes("\0") || !cwd.startsWith("/")) return undefined;
  const absolute = value.startsWith("/") ? value : `${cwd.replace(/\/+$/u, "")}/${value}`;
  const parts: string[] = [];
  for (const part of absolute.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}
