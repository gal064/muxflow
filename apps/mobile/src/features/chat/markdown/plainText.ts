// A reply's Markdown as the plain text it reads as, for the sheet where part
// of a message is selected and copied: no asterisks, backticks or fences, a
// list as its markers and lines, a table as its cells with a tab between them.

import { parseChatMarkdown, type ChatBlock, type ChatInline } from "./chatMarkdownModel";

const INDENT = "  ";

function runs(content: readonly ChatInline[]): string {
  return content.map((run) => run.text).join("");
}

/** Every line after the first moved in by `by`. */
function hang(text: string, by: string): string {
  return text.replace(/\n(?!\n)/g, `\n${by}`);
}

function block(value: ChatBlock): string {
  switch (value.kind) {
    case "paragraph":
    case "heading":
      return runs(value.content);
    case "code":
      return value.text.replace(/\n+$/, "");
    case "table":
      return [value.header, ...value.rows].map((row) => row.map(runs).join("\t")).join("\n");
    case "list":
      return value.items
        .map((item, index) => {
          const marker = item.checked === undefined ? (value.ordered ? `${value.start + index}. ` : "• ") : item.checked ? "[x] " : "[ ] ";
          return marker + hang(blocks(item.blocks, "\n"), INDENT);
        })
        .join("\n");
    case "quote":
      return blocks(value.blocks, "\n\n");
    case "rule":
      return "";
  }
}

function blocks(values: readonly ChatBlock[], between: string): string {
  return values.map(block).filter((said) => said !== "").join(between);
}

/** `source` without its Markdown. */
export function plainText(source: string): string {
  return blocks(parseChatMarkdown(source), "\n\n");
}
