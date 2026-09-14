export interface TerminalWebLink {
  text: string;
  /** Zero-based UTF-16 offsets into the rendered logical line. */
  start: number;
  end: number;
}

const WEB_URL = /https?:\/\/[^\s<>"']+/gu;
const TRAILING_SENTENCE_PUNCTUATION = new Set([".", ",", ";", ":", "!", "?", "`"]);
const CLOSING_DELIMITERS = new Map([
  [")", "("],
  ["]", "["],
  ["}", "{"],
]);

/** Finds HTTP(S) URLs without swallowing the prose punctuation after them. */
export function terminalWebLinks(line: string): TerminalWebLink[] {
  const links: TerminalWebLink[] = [];
  for (const match of line.matchAll(WEB_URL)) {
    const candidate = match[0];
    const length = terminalUrlLength(candidate);
    const text = candidate.slice(0, length);
    const start = match.index ?? 0;
    links.push({ text, start, end: start + text.length });
  }
  return links;
}

function terminalUrlLength(candidate: string): number {
  let end = candidate.length;
  while (end > 0) {
    const trailing = candidate.charAt(end - 1);
    if (TRAILING_SENTENCE_PUNCTUATION.has(trailing)) {
      end -= 1;
      continue;
    }
    const opening = CLOSING_DELIMITERS.get(trailing);
    if (!opening || delimiterCount(candidate, opening, end) >= delimiterCount(candidate, trailing, end)) break;
    end -= 1;
  }
  return end;
}

function delimiterCount(value: string, delimiter: string, end: number): number {
  let count = 0;
  for (let index = 0; index < end; index += 1) {
    if (value[index] === delimiter) count += 1;
  }
  return count;
}
