import { externalLinkTarget } from "../../ui/externalLinks";

export type MarkdownInline =
  | { kind: "text" | "strong" | "emphasis" | "code"; text: string }
  | { kind: "link"; text: string; href: string };

export type MarkdownBlock =
  | { kind: "paragraph" | "quote"; content: MarkdownInline[] }
  | { kind: "heading"; level: number; content: MarkdownInline[] }
  | { kind: "list"; ordered: boolean; items: MarkdownInline[][] }
  | { kind: "code"; text: string }
  | { kind: "rule" };

const HEADING = /^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/u;
const LIST_ITEM = /^\s{0,3}([-+*]|\d+[.)])\s+(.+)$/u;
const FENCE = /^\s{0,3}(`{3,}|~{3,})/u;
const RULE = /^\s{0,3}(?:(?:\*\s*){3,}|(?:-\s*){3,}|(?:_\s*){3,})$/u;

function isBlockStart(line: string): boolean {
  return HEADING.test(line) || LIST_ITEM.test(line) || FENCE.test(line) || RULE.test(line) || /^\s{0,3}>\s?/u.test(line);
}

/** A small native block model for the reply shapes agents commonly emit. */
export function parseVoiceMarkdown(source: string): MarkdownBlock[] {
  const lines = source.replace(/\r\n?/gu, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index]!;
    if (line.trim() === "") {
      index += 1;
      continue;
    }

    const fence = line.match(FENCE);
    if (fence) {
      const marker = fence[1]!;
      const body: string[] = [];
      index += 1;
      while (index < lines.length && !new RegExp(`^\\s{0,3}${marker[0]}{${marker.length},}\\s*$`, "u").test(lines[index]!)) {
        body.push(lines[index]!);
        index += 1;
      }
      if (index < lines.length) index += 1;
      blocks.push({ kind: "code", text: body.join("\n") });
      continue;
    }

    const heading = line.match(HEADING);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1]!.length, content: parseInlineMarkdown(heading[2]!) });
      index += 1;
      continue;
    }

    if (RULE.test(line)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }

    const item = line.match(LIST_ITEM);
    if (item) {
      const ordered = /^\d/u.test(item[1]!);
      const items: MarkdownInline[][] = [];
      while (index < lines.length) {
        const next = lines[index]!.match(LIST_ITEM);
        if (!next || /^\d/u.test(next[1]!) !== ordered) break;
        items.push(parseInlineMarkdown(next[2]!));
        index += 1;
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    if (/^\s{0,3}>\s?/u.test(line)) {
      const quote: string[] = [];
      while (index < lines.length) {
        const next = lines[index]!.match(/^\s{0,3}>\s?(.*)$/u);
        if (!next) break;
        quote.push(next[1]!);
        index += 1;
      }
      blocks.push({ kind: "quote", content: parseInlineMarkdown(quote.join(" ")) });
      continue;
    }

    const paragraph = [line.trim()];
    index += 1;
    while (index < lines.length && lines[index]!.trim() !== "" && !isBlockStart(lines[index]!)) {
      paragraph.push(lines[index]!.trim());
      index += 1;
    }
    blocks.push({ kind: "paragraph", content: parseInlineMarkdown(paragraph.join(" ")) });
  }
  return blocks;
}

/** Inline code, links and the two common emphasis forms; everything else remains literal and safe. */
export function parseInlineMarkdown(source: string): MarkdownInline[] {
  const spans: MarkdownInline[] = [];
  let plainStart = 0;
  const appendText = (text: string) => {
    const previous = spans.at(-1);
    if (previous?.kind === "text") previous.text += text;
    else if (text) spans.push({ kind: "text", text });
  };
  const flush = (end: number) => {
    appendText(source.slice(plainStart, end));
  };
  for (let index = 0; index < source.length;) {
    const character = source[index]!;
    const rest = character === "`" || character === "[" || character === "<" || character === "*" || character === "_"
      || ((character === "h" || character === "H") && /^https?:\/\//iu.test(source.slice(index, index + 8)))
      ? source.slice(index)
      : "";
    const code = character === "`" ? rest.match(/^`([^`]+)`/u) : null;
    const link = character === "[" ? markdownLink(rest) : undefined;
    const autoLink = character === "<" ? rest.match(/^<(https?:\/\/[^>]+)>/iu) : null;
    const bareLink = (character === "h" || character === "H") ? rest.match(/^https?:\/\/[^\s<]+/iu) : null;
    // CommonMark underscores cannot open emphasis inside a word. Agent replies
    // contain identifiers such as `voice_auto_play` often, and those
    // underscores must remain visible.
    const canOpenEmphasis = character === "*" || (character === "_" && (index === 0 || !/[\p{L}\p{N}]/u.test(source[index - 1]!)));
    let strong = canOpenEmphasis ? rest.match(character === "_" ? /^__(.+?)__/u : /^\*\*(.+?)\*\*/u) : null;
    let emphasis = canOpenEmphasis ? rest.match(character === "_" ? /^_([^_]+?)_/u : /^\*([^*]+?)\*/u) : null;
    if (character === "_") {
      if (strong && /[\p{L}\p{N}]/u.test(source[index + strong[0].length] ?? "")) strong = null;
      if (emphasis && /[\p{L}\p{N}]/u.test(source[index + emphasis[0].length] ?? "")) emphasis = null;
    }
    if (code) {
      flush(index);
      spans.push({ kind: "code", text: code[1]! });
      index += code[0].length;
    } else if (link) {
      flush(index);
      const href = externalLinkTarget(link.href);
      if (href) spans.push({ kind: "link", text: link.label, href });
      else appendText(link.label);
      index += link.length;
    } else if (autoLink) {
      flush(index);
      spans.push({ kind: "link", text: autoLink[1]!, href: autoLink[1]! });
      index += autoLink[0].length;
    } else if (bareLink) {
      flush(index);
      const href = bareLink[0].replace(/[.,;:!?]+$/u, "");
      spans.push({ kind: "link", text: href, href });
      index += href.length;
    } else if (strong) {
      flush(index);
      spans.push({ kind: "strong", text: strong[1]! });
      index += strong[0].length;
    } else if (emphasis) {
      flush(index);
      spans.push({ kind: "emphasis", text: emphasis[1]! });
      index += emphasis[0].length;
    } else {
      index += 1;
      continue;
    }
    plainStart = index;
  }
  flush(source.length);
  return spans;
}

function markdownLink(source: string): { label: string; href: string; length: number } | undefined {
  if (!source.startsWith("[")) return undefined;
  const labelEnd = source.indexOf("](", 1);
  if (labelEnd < 0) return undefined;
  let depth = 1;
  for (let index = labelEnd + 2; index < source.length; index += 1) {
    if (source[index] === "(") depth += 1;
    else if (source[index] === ")") {
      depth -= 1;
      if (depth === 0) {
        return { label: source.slice(1, labelEnd), href: source.slice(labelEnd + 2, index), length: index + 1 };
      }
    }
  }
  return undefined;
}
