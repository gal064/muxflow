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

  it("keeps line breaks in paragraphs and code", () => {
    expect(parseVoiceMarkdown("one\ntwo\n\n~~~\na\nb\n~~~")).toEqual([
      { kind: "paragraph", content: [{ kind: "text", text: "one\ntwo" }] },
      { kind: "code", text: "a\nb" },
    ]);
  });

  it("keeps the paragraphs and line breaks of a quoted email", () => {
    expect(parseVoiceMarkdown("> Subject: Hello\n>\n> Hi Sam,\n>\n> Thanks,\n> Gal\n>")).toEqual([
      { kind: "quote", content: [{ kind: "text", text: "Subject: Hello\n\nHi Sam,\n\nThanks,\nGal" }] },
    ]);
  });

  it("does not link an autolink that spans a line break", () => {
    expect(parseInlineMarkdown("<https://a.com\nb>")).toEqual([
      { kind: "text", text: "<" },
      { kind: "link", text: "https://a.com", href: "https://a.com" },
      { kind: "text", text: "\nb>" },
    ]);
  });

  it("keeps emphasis that spans a line break", () => {
    expect(parseVoiceMarkdown("**one\ntwo** and __three\nfour__")).toEqual([
      { kind: "paragraph", content: [{ kind: "strong", text: "one\ntwo" }, { kind: "text", text: " and " }, { kind: "strong", text: "three\nfour" }] },
    ]);
  });

  it("keeps intraword and leading underscores in identifiers while styling delimited emphasis", () => {
    expect(parseInlineMarkdown("voice_auto_play and _private_field; then _natural speech_")).toEqual([
      { kind: "text", text: "voice_auto_play and _private_field; then " },
      { kind: "emphasis", text: "natural speech" },
    ]);
  });
});
