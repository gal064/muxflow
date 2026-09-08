import { describe, expect, it } from "vitest";

import { parseInlineMarkdown, parseVoiceMarkdown } from "./markdownModel";

describe("voice Markdown model", () => {
  it("preserves headings, lists and fenced code as distinct display blocks", () => {
    expect(parseVoiceMarkdown("# Result\n\n- first\n- **second**\n\n```ts\nconst ok = true;\n```" )).toEqual([
      { kind: "heading", level: 1, content: [{ kind: "text", text: "Result" }] },
      { kind: "list", ordered: false, items: [[{ kind: "text", text: "first" }], [{ kind: "strong", text: "second" }]] },
      { kind: "code", text: "const ok = true;" },
    ]);
  });

  it("makes only external links interactive and keeps unsafe or relative labels as text", () => {
    expect(parseInlineMarkdown("[docs](https://example.com) [bad](javascript:alert(1)) [local](./README.md)")).toEqual([
      { kind: "link", text: "docs", href: "https://example.com" },
      { kind: "text", text: " bad local" },
    ]);
  });

  it("keeps paragraph soft-wraps natural and preserves code newlines", () => {
    expect(parseVoiceMarkdown("one\ntwo\n\n~~~\na\nb\n~~~")).toEqual([
      { kind: "paragraph", content: [{ kind: "text", text: "one two" }] },
      { kind: "code", text: "a\nb" },
    ]);
  });

  it("keeps intraword and leading underscores in identifiers while styling delimited emphasis", () => {
    expect(parseInlineMarkdown("voice_auto_play and _private_field; then _natural speech_")).toEqual([
      { kind: "text", text: "voice_auto_play and _private_field; then " },
      { kind: "emphasis", text: "natural speech" },
    ]);
  });
});
