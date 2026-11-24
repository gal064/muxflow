import { describe, expect, it } from "vitest";

import { columnWidth, fileName, parseChatMarkdown, splitPosition, streamingCodeText, type ChatBlock, type ChatCodeBlock } from "./chatMarkdownModel";

function only(source: string): ChatBlock {
  const blocks = parseChatMarkdown(source);
  expect(blocks).toHaveLength(1);
  return blocks[0]!;
}

describe("chat Markdown blocks", () => {
  it("reads a paragraph's inline styles as runs", () => {
    expect(only("Plain **bold** *em* ~~gone~~ `x<y&amp;` and ***both***")).toEqual({
      kind: "paragraph",
      content: [
        { text: "Plain " },
        { text: "bold", strong: true },
        { text: " " },
        { text: "em", emphasis: true },
        { text: " " },
        { text: "gone", strike: true },
        { text: " " },
        { text: "x<y&amp;", code: true },
        { text: " and " },
        { text: "both", emphasis: true, strong: true },
      ],
    });
  });

  it("keeps line breaks, hard or not, and shows raw HTML and escapes as text", () => {
    expect(only("one  \ntwo\nthree <b>raw</b> \\*star\\* a &lt; b &amp; &#35;&#x41; &unknown;")).toEqual({
      kind: "paragraph",
      content: [{ text: "one\ntwo\nthree <b>raw</b> *star* a < b & #A &unknown;" }],
    });
  });

  it("reads a raw <br> as a line break, in a paragraph and in a table cell", () => {
    expect(only("one<br>two<br/>three<BR />four")).toEqual({ kind: "paragraph", content: [{ text: "one\ntwo\nthree\nfour" }] });
    // At the end of a source line it is one break, not two; inside a style it keeps the style.
    expect(only("one<br>\ntwo **bold<br>still**")).toEqual({
      kind: "paragraph",
      content: [{ text: "one\ntwo " }, { text: "bold\nstill", strong: true }],
    });
    // In code it is the text that was written, and other tags still are too.
    expect(only("`a<br>b` <br> <b>raw</b>")).toEqual({
      kind: "paragraph",
      content: [{ text: "a<br>b", code: true }, { text: " \n <b>raw</b>" }],
    });
    const table = only("| Step | Notes |\n| --- | --- |\n| Build | cargo build<br>cargo test |\n| Ship | tag<br/>**push** |\n");
    expect(table).toMatchObject({
      kind: "table",
      rows: [
        [[{ text: "Build" }], [{ text: "cargo build\ncargo test" }]],
        [[{ text: "Ship" }], [{ text: "tag\n" }, { text: "push", strong: true }]],
      ],
    });
    // A column is as wide as its longest line, not as the lines put end to end.
    expect(table).toMatchObject({ chars: [5, "cargo build".length] });
    // Standing alone between two blocks it has no line to break.
    expect(parseChatMarkdown("first\n\n<br>\n\nsecond")).toEqual([
      { kind: "paragraph", content: [{ text: "first" }] },
      { kind: "paragraph", content: [{ text: "second" }] },
    ]);
  });

  it("reads headings of every level", () => {
    const blocks = parseChatMarkdown("# One\n\n## Two\n\n###### Six `code`\n\nSetext\n---\n");
    expect(blocks).toEqual([
      { kind: "heading", level: 1, content: [{ text: "One" }] },
      { kind: "heading", level: 2, content: [{ text: "Two" }] },
      { kind: "heading", level: 6, content: [{ text: "Six " }, { text: "code", code: true }] },
      { kind: "heading", level: 2, content: [{ text: "Setext" }] },
    ]);
  });

  it("reads fenced and indented code", () => {
    expect(parseChatMarkdown("```rust title=join.rs\nfn join() {\n    rows\n}\n```\n\n    indented\n    code\n\n~~~\nplain *text*\n~~~\n")).toEqual([
      { kind: "code", language: "rust", text: "fn join() {\n    rows\n}", closed: true },
      { kind: "code", language: "", text: "indented\ncode", closed: true },
      { kind: "code", language: "", text: "plain *text*", closed: true },
    ]);
  });

  it("reads a fence with no closing line as code that is still open", () => {
    expect(parseChatMarkdown("Here:\n\n```sh\nls -la\n")).toEqual([
      { kind: "paragraph", content: [{ text: "Here:" }] },
      { kind: "code", language: "sh", text: "ls -la", closed: false },
    ]);
    expect(only("```py")).toEqual({ kind: "code", language: "py", text: "", closed: false });
    expect(only("````md\n```\ninner\n```\n")).toEqual({ kind: "code", language: "md", text: "```\ninner\n```", closed: false });
    expect(only("````md\n```\ninner\n```\n````")).toEqual({ kind: "code", language: "md", text: "```\ninner\n```", closed: true });
  });

  it("leaves the start of a closing fence out of code that is still being written", () => {
    const open = only("```js\nlet a = 1;\n``") as ChatCodeBlock;
    expect(open.text).toBe("let a = 1;\n``");
    expect(streamingCodeText(open)).toBe("let a = 1;");
    const closed = only("````js\nlet a = 1;\n``\n````") as ChatCodeBlock;
    expect(streamingCodeText(closed)).toBe("let a = 1;\n``");
  });

  it("reads a table with its alignments and inline styles", () => {
    expect(only("| Case | Joined | Why |\n| :-- | :-: | --: |\n| Soft **wrap** | yes | The terminal marks it |\n| Boxed | `no` |\n")).toEqual({
      kind: "table",
      align: ["left", "center", "right"],
      chars: [9, 6, 21],
      header: [[{ text: "Case" }], [{ text: "Joined" }], [{ text: "Why" }]],
      rows: [
        [[{ text: "Soft " }, { text: "wrap", strong: true }], [{ text: "yes" }], [{ text: "The terminal marks it" }]],
        [[{ text: "Boxed" }], [{ text: "no", code: true }], []],
      ],
    });
    expect(only("| a | b |\n| - | - |\n")).toMatchObject({ kind: "table", align: ["left", "left"], rows: [] });
    // Without its second line a table is not one yet.
    expect(only("| a | b |")).toMatchObject({ kind: "paragraph" });
  });

  it("sizes a column from its longest cell, within a least and a most", () => {
    expect(columnWidth(1)).toBe(56);
    expect(columnWidth(12)).toBe(110);
    expect(columnWidth(12, 1.3)).toBe(137);
    expect(columnWidth(400)).toBe(220);
  });

  it("reads nested lists and keeps an ordered list's first number", () => {
    expect(only("3. Strip the border\n   - left edge\n     - deep\n   - right edge\n4. Join the rows\n")).toEqual({
      kind: "list",
      ordered: true,
      start: 3,
      items: [
        {
          blocks: [
            { kind: "paragraph", content: [{ text: "Strip the border" }] },
            {
              kind: "list",
              ordered: false,
              start: 1,
              items: [
                {
                  blocks: [
                    { kind: "paragraph", content: [{ text: "left edge" }] },
                    { kind: "list", ordered: false, start: 1, items: [{ blocks: [{ kind: "paragraph", content: [{ text: "deep" }] }] }] },
                  ],
                },
                { blocks: [{ kind: "paragraph", content: [{ text: "right edge" }] }] },
              ],
            },
          ],
        },
        { blocks: [{ kind: "paragraph", content: [{ text: "Join the rows" }] }] },
      ],
    });
  });

  it("reads a list item made of several blocks", () => {
    expect(only("1. Run it:\n\n   ```sh\n   pnpm test\n   ```\n\n   Then read the output.\n")).toEqual({
      kind: "list",
      ordered: true,
      start: 1,
      items: [{
        blocks: [
          { kind: "paragraph", content: [{ text: "Run it:" }] },
          { kind: "code", language: "sh", text: "pnpm test", closed: true },
          { kind: "paragraph", content: [{ text: "Then read the output." }] },
        ],
      }],
    });
  });

  it("reads task items, in a tight list and in one with blank lines", () => {
    expect(only("- [ ] todo\n- [x] done **now**\n- plain\n")).toEqual({
      kind: "list",
      ordered: false,
      start: 1,
      items: [
        { checked: false, blocks: [{ kind: "paragraph", content: [{ text: "todo" }] }] },
        { checked: true, blocks: [{ kind: "paragraph", content: [{ text: "done " }, { text: "now", strong: true }] }] },
        { blocks: [{ kind: "paragraph", content: [{ text: "plain" }] }] },
      ],
    });
    expect(only("- [x] a\n\n- [ ] b\n")).toMatchObject({
      items: [
        { checked: true, blocks: [{ kind: "paragraph", content: [{ text: "a" }] }] },
        { checked: false, blocks: [{ kind: "paragraph", content: [{ text: "b" }] }] },
      ],
    });
  });

  it("reads a quote with the blocks inside it, and a rule", () => {
    expect(parseChatMarkdown("> Quoted *text*\n>\n> - item\n\n---\n")).toEqual([
      {
        kind: "quote",
        blocks: [
          { kind: "paragraph", content: [{ text: "Quoted " }, { text: "text", emphasis: true }] },
          { kind: "list", ordered: false, start: 1, items: [{ blocks: [{ kind: "paragraph", content: [{ text: "item" }] }] }] },
        ],
      },
      { kind: "rule" },
    ]);
  });

  it("stops nesting at a depth a phone can draw, and shows the rest as written", () => {
    let level = only(`${">".repeat(12)} deep`);
    let depth = 0;
    while (level.kind === "quote") {
      depth += 1;
      level = level.blocks[0]!;
    }
    expect(depth).toBe(8);
    expect(level).toEqual({ kind: "paragraph", content: [{ text: ">>>> deep" }] });
    // Nesting deep enough to exhaust marked's own stack is still shown, as one block.
    expect(parseChatMarkdown(`${">".repeat(30_000)} x`)).toHaveLength(1);
  });

  it("shows an HTML block as the text it is", () => {
    expect(parseChatMarkdown("<div class=\"x\">\n<script>alert(1)</script>\n</div>\n\nafter")).toEqual([
      { kind: "paragraph", content: [{ text: "<div class=\"x\">\n<script>alert(1)</script>\n</div>" }] },
      { kind: "paragraph", content: [{ text: "after" }] },
    ]);
  });

  it("draws an image as a link named by its alt text", () => {
    expect(only("![A *chart*](https://example.com/c.png \"t\") ![](https://example.com/bare.png) ![local](./shot.png)")).toEqual({
      kind: "paragraph",
      content: [
        { text: "A ", href: "https://example.com/c.png" },
        { text: "chart", emphasis: true, href: "https://example.com/c.png" },
        { text: " " },
        { text: "https://example.com/bare.png", href: "https://example.com/bare.png" },
        { text: " local" },
      ],
    });
  });
});

describe("chat Markdown links", () => {
  it("keeps the links the phone may open", () => {
    expect(only("[docs](https://example.com/a_(b)) <https://auto.example> https://bare.example/x?y=1&z=2. www.example.org me@example.com [mail](mailto:a@example.test) [**bold** ref][r]\n\n[r]: http://ref.example\n")).toEqual({
      kind: "paragraph",
      content: [
        { text: "docs", href: "https://example.com/a_(b)" },
        { text: " " },
        { text: "https://auto.example", href: "https://auto.example" },
        { text: " " },
        { text: "https://bare.example/x?y=1&z=2", href: "https://bare.example/x?y=1&z=2" },
        { text: ". " },
        { text: "www.example.org", href: "http://www.example.org" },
        { text: " " },
        { text: "me@example.com", href: "mailto:me@example.com" },
        { text: " " },
        { text: "mail", href: "mailto:a@example.test" },
        { text: " " },
        { text: "bold", strong: true, href: "http://ref.example" },
        { text: " ref", href: "http://ref.example" },
      ],
    });
  });

  it("turns every other link into its label", () => {
    const block = only("[js](javascript:alert(1)) [file](file:///etc/passwd) [top](#top) [cdn](//cdn.example/x.js) [intent](intent://x#Intent;end) [data](data:text/html,x) [tel](tel:+1-555-0100) [app](muxflow://open) [*styled*](vbscript:x)");
    expect(block).toEqual({
      kind: "paragraph",
      content: [{ text: "js file top cdn intent data tel app " }, { text: "styled", emphasis: true }],
    });
  });

  it("reads a link to a path as a file link, its label the file's name when it is the path", () => {
    expect(only("In [the join](src/links.rs:171), [crates/term/links.rs:12](crates/term/links.rs:12), [](/etc/hosts) and [*it*](<docs/a b.md>)")).toEqual({
      kind: "paragraph",
      content: [
        { text: "In " },
        { text: "the join", file: "src/links.rs" },
        { text: ", " },
        { text: "links.rs:12", file: "crates/term/links.rs" },
        { text: ", " },
        { text: "hosts", file: "/etc/hosts" },
        { text: " and " },
        { text: "it", emphasis: true, file: "docs/a b.md" },
      ],
    });
  });

  it("marks inline code that could name a file, and only that", () => {
    expect(only("`/home/me/tmp/pr-review.md` `src/a.rs:12:3` `cargo test` `/` `1/2` `a//b` `dir/` `https://x.dev/a` `a b/c`")).toEqual({
      kind: "paragraph",
      content: [
        { text: "/home/me/tmp/pr-review.md", code: true, path: "/home/me/tmp/pr-review.md" },
        { text: " " },
        { text: "src/a.rs:12:3", code: true, path: "src/a.rs:12:3" },
        { text: " " },
        { text: "cargo test", code: true },
        { text: " " },
        { text: "/", code: true },
        { text: " " },
        { text: "1/2", code: true },
        { text: " " },
        { text: "a//b", code: true },
        { text: " " },
        { text: "dir/", code: true },
        { text: " " },
        { text: "https://x.dev/a", code: true },
        { text: " " },
        { text: "a b/c", code: true },
      ],
    });
  });

  it("splits a line and column off a path, and names its file", () => {
    expect(splitPosition("src/a.rs:12:3")).toEqual(["src/a.rs", ":12:3"]);
    expect(splitPosition("src/a.rs:12")).toEqual(["src/a.rs", ":12"]);
    expect(splitPosition("a:1:2:3")).toEqual(["a:1", ":2:3"]);
    expect(splitPosition("src/a.rs")).toEqual(["src/a.rs", ""]);
    expect(fileName("crates/term/links.rs:12")).toBe("links.rs:12");
    expect(fileName("/etc/hosts")).toBe("hosts");
  });
});

describe("chat Markdown parsing is kept", () => {
  it("returns the same blocks for the same source", () => {
    const source = "Kept **once**\n\n- a\n- b\n";
    const first = parseChatMarkdown(source);
    expect(parseChatMarkdown(source)).toBe(first);
    parseChatMarkdown("Something else entirely");
    expect(parseChatMarkdown(`${"Kept **once**"}\n\n- a\n- b\n`)).toBe(first);
  });

  it("keeps the blocks a growing reply already had", () => {
    const first = parseChatMarkdown("Growing reply\n\n```ts\nconst a = 1;\n```\n\nThen a li");
    const second = parseChatMarkdown("Growing reply\n\n```ts\nconst a = 1;\n```\n\nThen a list:\n\n- one");
    expect(second).toHaveLength(4);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).toBe(first[1]);
    expect(second[2]).not.toBe(first[2]);
    expect(second[2]).toEqual({ kind: "paragraph", content: [{ text: "Then a list:" }] });
  });

  it("reads every block again when a link definition arrives", () => {
    const first = parseChatMarkdown("See [the plan][plan] first.\n\nMore text");
    expect(first[0]).toEqual({ kind: "paragraph", content: [{ text: "See [the plan][plan] first." }] });
    const second = parseChatMarkdown("See [the plan][plan] first.\n\nMore text\n\n[plan]: https://example.com/plan");
    expect(second[0]).toEqual({
      kind: "paragraph",
      content: [{ text: "See " }, { text: "the plan", href: "https://example.com/plan" }, { text: " first." }],
    });
  });

  it("keeps many replies, and reads the oldest again once it has gone", () => {
    const first = parseChatMarkdown("Reply number zero");
    for (let index = 1; index <= 127; index += 1) parseChatMarkdown(`Another reply, ${index}`);
    expect(parseChatMarkdown("Reply number zero")).toBe(first);
    for (let index = 0; index < 128; index += 1) parseChatMarkdown(`A later reply, ${index}`);
    const again = parseChatMarkdown("Reply number zero");
    expect(again).not.toBe(first);
    expect(again).toEqual(first);
  });
});
