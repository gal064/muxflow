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

const TRAILING_PUNCTUATION = new Set([",", ";", ":", "!", "?", "."]);
const NON_FILE_PREFIX = /^(?:\/\/|[^/(){}]*:|(?:www\.[^/()[\]{}]*|[a-z\d-]+(?:\.[a-z\d-]+)+)\/)/iu;
const CLOSING_WRAPPERS = new Map([
  [")", "("],
  ["]", "["],
  ["}", "{"],
]);

/**
 * Finds only the deliberately small P0 path vocabulary in one terminal line.
 *
 * Whitespace, quotes, and prose delimiters separate tokens. Balanced delimiters
 * inside path components remain part of the filename. Absolute paths and
 * relative paths with a slash qualify; a trailing line/column location is left
 * outside the link. URLs and bare filenames do not qualify.
 */
export function terminalFileLinks(line: string): TerminalFileLink[] {
  const links: TerminalFileLink[] = [];
  for (const match of filePathTokens(line)) {
    const token = match.text;
    let trailing = token.length;
    while (trailing > 0 && TRAILING_PUNCTUATION.has(token.charAt(trailing - 1))) trailing -= 1;
    const wrappedText = token.slice(0, trailing);
    const location = /:\d+(?::\d+)?$/u.exec(wrappedText);
    const text = location ? wrappedText.slice(0, location.index) : wrappedText;
    if (!isExplicitTerminalFilePath(text)) continue;
    const start = match.start;
    links.push({ text, start, end: start + text.length });
  }
  return links;
}

/** Splits attached labels and unmatched delimiters without losing valid filenames. */
function* filePathTokens(line: string): Generator<{ text: string; start: number }> {
  for (const match of line.matchAll(/[^\s"'`]+/gu)) {
    const token = match[0];
    let firstSlash = token.indexOf("/");
    const paired = new Map<number, number>();
    const openings: number[] = [];
    for (let index = 0; index < token.length; index += 1) {
      const char = token.charAt(index);
      if (char === "(" || char === "[" || char === "{") openings.push(index);
      const opening = CLOSING_WRAPPERS.get(char);
      if (!opening) continue;
      const start = openings.pop();
      if (start !== undefined && token.charAt(start) === opening) paired.set(start, index);
      else openings.length = 0;
    }

    let start = 0;
    let hasSlash = false;
    for (let index = 0; index < token.length; index += 1) {
      // A URL or host reference owns the rest of its token. Splitting its
      // unmatched delimiters must not create separate file targets.
      if (index === start && NON_FILE_PREFIX.test(token.slice(start))) {
        start = token.length;
        break;
      }
      const char = token.charAt(index);
      if (char === "/") hasSlash = true;
      if (char !== "(" && char !== "[" && char !== "{" && !CLOSING_WRAPPERS.has(char)) continue;
      const closing = paired.get(index);
      if (closing !== undefined && (hasSlash || closing < firstSlash)) {
        index = closing;
        continue;
      }
      if (start < index) yield { text: token.slice(start, index), start: match.index + start };
      start = index + 1;
      firstSlash = token.indexOf("/", start);
      hasSlash = false;
    }
    if (start < token.length) yield { text: token.slice(start), start: match.index + start };
  }
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
