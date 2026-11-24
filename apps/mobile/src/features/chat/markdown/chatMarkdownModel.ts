// What a chat reply's Markdown is made of, as the phone draws it: blocks
// (paragraph, heading, code, table, list, quote, rule) holding runs of styled
// text. `marked` reads the source; nothing outside this file sees its tokens.

import { Lexer, type MarkedToken, type Token } from "marked";

import { externalLinkTarget } from "../../../ui/externalLinks";

/**
 * One run of text with the styles that apply to it. `href` is set only for a
 * link the phone may open (see `externalLinkTarget`); `file` for a Markdown
 * link to a path on the host, relative to the agent's directory unless
 * absolute; `path` for inline code that could name a file, which is a link
 * once the host says the file is there. Any other link is its label as plain
 * text.
 */
export type ChatInline = {
  text: string;
  strong?: true;
  emphasis?: true;
  strike?: true;
  code?: true;
  href?: string;
  file?: string;
  path?: string;
};

export type ChatAlign = "left" | "center" | "right";

/** `checked` is set for a task item (`- [ ]`, `- [x]`) and absent for any other. */
export type ChatListItem = { checked?: boolean; blocks: ChatBlock[] };

export type ChatCodeBlock = {
  kind: "code";
  /** The first word after the opening fence, or "". */
  language: string;
  text: string;
  /** False for a fence with no closing line yet: a reply still being written. */
  closed: boolean;
};

export type ChatTableBlock = {
  kind: "table";
  align: ChatAlign[];
  /** Per column, the length of its longest cell in characters (see `columnWidth`). */
  chars: number[];
  header: ChatInline[][];
  rows: ChatInline[][][];
};

export type ChatBlock =
  | { kind: "paragraph"; content: ChatInline[] }
  | { kind: "heading"; level: number; content: ChatInline[] }
  | ChatCodeBlock
  | ChatTableBlock
  | { kind: "list"; ordered: boolean; start: number; items: ChatListItem[] }
  | { kind: "quote"; blocks: ChatBlock[] }
  | { kind: "rule" };

type Marks = Omit<ChatInline, "text">;

const NO_MARKS: Marks = {};

// marked leaves character references for an HTML renderer to resolve. These
// are the ones replies use; any other name stays as written.
const ENTITIES = new Map<string, string>([
  ["amp", "&"], ["lt", "<"], ["gt", ">"], ["quot", "\""], ["apos", "'"], ["nbsp", " "],
  ["copy", "©"], ["reg", "®"], ["trade", "™"], ["hellip", "…"], ["mdash", "—"], ["ndash", "–"],
  ["lsquo", "‘"], ["rsquo", "’"], ["ldquo", "“"], ["rdquo", "”"], ["bull", "•"], ["middot", "·"],
  ["larr", "←"], ["rarr", "→"], ["uarr", "↑"], ["darr", "↓"], ["times", "×"], ["deg", "°"],
]);

function decodeEntities(text: string): string {
  if (!text.includes("&")) return text;
  return text.replace(/&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([a-zA-Z][a-zA-Z0-9]{1,31}));/gu, (whole: string, decimal?: string, hex?: string, name?: string) => {
    if (name !== undefined) return ENTITIES.get(name) ?? whole;
    const code = decimal !== undefined ? Number(decimal) : parseInt(hex!, 16);
    return code > 0 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff) ? String.fromCodePoint(code) : "�";
  });
}

function sameMarks(a: Marks, b: Marks): boolean {
  return a.strong === b.strong && a.emphasis === b.emphasis && a.strike === b.strike && a.code === b.code && a.href === b.href
    && a.file === b.file && a.path === b.path;
}

function push(out: ChatInline[], text: string, marks: Marks): void {
  if (!text) return;
  const previous = out.at(-1);
  // Each path stays its own run, even next to the same path.
  if (previous && previous.path === undefined && sameMarks(previous, marks)) previous.text += text;
  else out.push({ text, ...marks });
}

/** `<br>`, `<br/>`, `<br />`: the one piece of raw HTML replies use, mostly to break a line inside a table cell. */
const LINE_BREAK_TAG = /^<br\s*\/?>$/iu;

/** Flattens marked's nested inline tokens into runs, each carrying every style around it. */
function inlines(tokens: Token[] | undefined, marks: Marks = NO_MARKS, out: ChatInline[] = []): ChatInline[] {
  // A `<br>` at the end of a source line: the line's own end is not a second break.
  let broke = false;
  for (const token of (tokens ?? []) as MarkedToken[]) {
    const afterBreak = broke;
    broke = false;
    switch (token.type) {
      case "strong":
        inlines(token.tokens, { ...marks, strong: true }, out);
        break;
      case "em":
        inlines(token.tokens, { ...marks, emphasis: true }, out);
        break;
      case "del":
        inlines(token.tokens, { ...marks, strike: true }, out);
        break;
      case "codespan": {
        const { text } = token;
        push(out, text, pathLike(text) ? { ...marks, code: true, path: text } : { ...marks, code: true });
        break;
      }
      case "br":
        push(out, "\n", marks);
        break;
      case "link":
      case "image": {
        if (token.type === "link" && isFileTarget(token.href)) {
          const file = splitPosition(token.href)[0];
          const label = token.tokens.map((inner) => inner.raw).join("");
          // A label that is the path itself is shown as the file's name.
          if (label.includes("/") || !label.trim()) push(out, fileName(token.href), { ...marks, file });
          else inlines(token.tokens, { ...marks, file }, out);
          break;
        }
        // An image is never loaded: its alt text stands in, as a link to it.
        const href = externalLinkTarget(token.href);
        const inner = href ? { ...marks, href } : marks;
        if (token.tokens.length > 0) inlines(token.tokens, inner, out);
        else push(out, token.href, inner);
        break;
      }
      case "escape":
        push(out, token.text, marks);
        break;
      case "text":
        if (token.tokens) {
          inlines(token.tokens, marks, out);
        } else {
          const text = token.escaped ? token.text : decodeEntities(token.text);
          push(out, afterBreak && text.startsWith("\n") ? text.slice(1) : text, marks);
        }
        break;
      case "checkbox":
        break;
      case "html":
        if (LINE_BREAK_TAG.test(token.raw.trim())) {
          push(out, "\n", marks);
          broke = true;
          break;
        }
        // Any other raw HTML is shown as it was written.
        push(out, token.raw, marks);
        break;
      default:
        // Anything marked adds later is shown as it was written.
        push(out, token.raw, marks);
    }
  }
  return out;
}

/** Splits `path:line` or `path:line:column` into the path and that suffix. */
export function splitPosition(path: string): [path: string, position: string] {
  const match = /^(.*?)((?::\d+){1,2})$/su.exec(path);
  return match && match[1] ? [match[1], match[2]!] : [path, ""];
}

/** The file's name, and the line it points at if it does: `a/b/c.rs:12` is `c.rs:12`. */
export function fileName(path: string): string {
  const [file, position] = splitPosition(path);
  return `${file.slice(file.lastIndexOf("/") + 1)}${position}`;
}

/**
 * Inline code that could name a file: one word with a separator in it. The
 * host says whether it does. The same rule as the desktop app's.
 */
export function pathLike(code: string): boolean {
  const [path] = splitPosition(code);
  return path.length <= 512
    && path.includes("/")
    && !path.endsWith("/")
    && !path.includes("//")
    && /\p{L}/u.test(path)
    && /^[\p{L}\p{N}/._\-~+@,]+$/u.test(path);
}

/** A Markdown link's target that is a path: no scheme, and not an anchor. */
function isFileTarget(target: string): boolean {
  const colon = target.indexOf(":");
  const scheme = colon > 1
    && /^[A-Za-z0-9+.-]+$/u.test(target.slice(0, colon))
    && !/^[\d:]*$/u.test(target.slice(colon + 1));
  return target !== "" && !scheme && !target.startsWith("#") && !target.startsWith("//")
    && !/[\u0000-\u001f\u007f]/u.test(target);
}

/** The length of a cell's longest line, for sizing its column; a wide character (CJK, emoji) counts as two. */
function cellChars(cell: ChatInline[]): number {
  let longest = 0;
  let count = 0;
  for (const run of cell) {
    for (const character of run.text) {
      count = character === "\n" ? 0 : count + (character.codePointAt(0)! >= 0x2e80 ? 2 : 1);
      longest = Math.max(longest, count);
    }
  }
  return longest;
}

function fenceClosed(raw: string): boolean {
  const open = /^ {0,3}(`{3,}|~{3,})/u.exec(raw)?.[1];
  if (!open) return true;
  const lines = raw.replace(/\n+$/u, "").split("\n");
  const last = lines.at(-1)!.trim();
  return lines.length > 1 && last.startsWith(open) && /^[`~]*$/u.test(last);
}

function paragraph(content: ChatInline[]): ChatBlock | undefined {
  return content.length > 0 ? { kind: "paragraph", content } : undefined;
}

/**
 * Lists and quotes nest this deep at most. A phone has no width left for
 * more, and a view tree hundreds deep does not survive layout; what lies
 * deeper is shown as the text it was written as.
 */
const NESTING_LIMIT = 8;

function block(token: MarkedToken, depth: number): ChatBlock | undefined {
  if ((token.type === "list" || token.type === "blockquote") && depth >= NESTING_LIMIT) {
    return paragraph([{ text: token.raw.replace(/\n+$/u, "") }]);
  }
  switch (token.type) {
    case "space":
    case "def":
    case "checkbox":
      return undefined;
    case "paragraph":
      return paragraph(inlines(token.tokens));
    case "heading":
      return { kind: "heading", level: token.depth, content: inlines(token.tokens) };
    case "code":
      return {
        kind: "code",
        language: token.lang?.trim().split(/\s+/u)[0] ?? "",
        text: token.text,
        closed: token.codeBlockStyle === "indented" || fenceClosed(token.raw),
      };
    case "table": {
      const header = token.header.map((cell) => inlines(cell.tokens));
      const rows = token.rows.map((row) => row.map((cell) => inlines(cell.tokens)));
      return {
        kind: "table",
        align: token.align.map((align) => align ?? "left"),
        chars: header.map((cell, column) => rows.reduce((longest, row) => Math.max(longest, cellChars(row[column] ?? [])), cellChars(cell))),
        header,
        rows,
      };
    }
    case "list":
      return {
        kind: "list",
        ordered: token.ordered,
        start: token.ordered && typeof token.start === "number" ? token.start : 1,
        items: token.items.map((item) => ({ ...(item.task ? { checked: item.checked === true } : {}), blocks: blocks(item.tokens, depth + 1) })),
      };
    case "blockquote":
      return { kind: "quote", blocks: blocks(token.tokens, depth + 1) };
    case "hr":
      return { kind: "rule" };
    case "html": {
      const text = token.text.replace(/\n+$/u, "");
      // Line breaks standing alone between two blocks have no line to break.
      if (/^(?:<br\s*\/?>\s*)+$/iu.test(text)) return undefined;
      return paragraph(text ? [{ text }] : []);
    }
    default:
      // A list item's own line arrives as inline tokens with no paragraph around them.
      return paragraph(inlines([token]));
  }
}

function blocks(tokens: Token[], depth: number): ChatBlock[] {
  const out: ChatBlock[] = [];
  for (const token of tokens as MarkedToken[]) {
    const next = block(token, depth);
    if (next) out.push(next);
  }
  return out;
}

/** One parsed reply, with what each top-level block was read from. */
type Parsed = { blocks: ChatBlock[]; types: string[]; raws: string[]; definitions: string };

/**
 * Reads `source`. The leading top-level blocks whose source is the same as in
 * `earlier` are those same objects again, so a reply that grew by a line
 * re-renders only its last block. Link definitions (`[name]: url`) reach
 * every block, so nothing is kept across a change in them.
 */
function read(source: string, earlier: Parsed | undefined): Parsed {
  try {
    const tokens = Lexer.lex(source, { gfm: true, breaks: false });
    const definitions = JSON.stringify(tokens.links);
    let kept = earlier?.definitions === definitions ? earlier : undefined;
    const parsed: Parsed = { blocks: [], types: [], raws: [], definitions };
    for (const token of tokens as MarkedToken[]) {
      if (token.type === "space" || token.type === "def") continue;
      const at = parsed.blocks.length;
      if (kept && (kept.types[at] !== token.type || kept.raws[at] !== token.raw)) kept = undefined;
      const next = kept ? kept.blocks[at] : block(token, 0);
      if (!next) continue;
      parsed.blocks.push(next);
      parsed.types.push(token.type);
      parsed.raws.push(token.raw);
    }
    return parsed;
  } catch {
    // marked gives up on some input, nesting thousands deep for one. The reply is still shown, as plain text.
    return { blocks: [{ kind: "paragraph", content: [{ text: source }] }], types: [], raws: [], definitions: "" };
  }
}

/** Replies kept parsed, least recently used first. A row scrolled back into view does not parse again. */
const PARSED_LIMIT = 128;
const parsedBySource = new Map<string, Parsed>();
let lastRead: string | undefined;

/**
 * The blocks of a reply. The same `source` returns the same array, and the
 * blocks are shared between calls: treat them as read-only.
 */
export function parseChatMarkdown(source: string): ChatBlock[] {
  const hit = parsedBySource.get(source);
  if (hit) {
    parsedBySource.delete(source);
    parsedBySource.set(source, hit);
    return hit.blocks;
  }
  // A reply being written arrives as its earlier text plus more. The earlier
  // text is not drawn again, so it gives its place to the longer one.
  const earlier = lastRead !== undefined && source.startsWith(lastRead) ? parsedBySource.get(lastRead) : undefined;
  if (earlier) parsedBySource.delete(lastRead!);
  const parsed = read(source, earlier);
  parsedBySource.set(source, parsed);
  lastRead = source;
  if (parsedBySource.size > PARSED_LIMIT) parsedBySource.delete(parsedBySource.keys().next().value!);
  return parsed.blocks;
}

/**
 * The code to draw for a block at the end of a reply that is still being
 * written. The first characters of the closing fence arrive as a last line of
 * code before the fence is complete; that line is left out.
 */
export function streamingCodeText(code: ChatCodeBlock): string {
  return code.closed ? code.text : code.text.replace(/(?:^|\n) {0,3}(?:`+|~+)$/u, "");
}

/** The space on each side of a table cell's text, in dp. */
export const TABLE_CELL_PADDING = 10;
// An average character of the 13 sp table text, with room to spare.
const CHARACTER_WIDTH = 7.5;
const COLUMN_MIN = 56;
const COLUMN_MAX = 220;

/**
 * A table column's width in dp from its longest cell, so every row lines up
 * without measuring text. A cell longer than the widest column wraps.
 */
export function columnWidth(chars: number, fontScale = 1): number {
  return Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, Math.ceil(chars * CHARACTER_WIDTH * fontScale) + 2 * TABLE_CELL_PADDING));
}
