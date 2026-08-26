// @vitest-environment jsdom
//
// `renderSafeMarkdown` is the desktop's function, verbatim (D4). It needs a
// DOM — `DOMParser`, `document` — which is what the WebView gives it on the
// phone and what jsdom gives it here.

import { describe, expect, it } from "vitest";
import { renderSafeMarkdown } from "./markdown";

describe("what the sanitiser strips (§10.3, D4)", () => {
  it("removes scripts, iframes and inline event handlers", () => {
    const html = renderSafeMarkdown(
      ["<script>window.stolen = 1</script>", "<iframe src=\"https://evil.example\"></iframe>", "<img src=x onerror=\"alert(1)\">"].join("\n\n"),
    );
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<iframe");
    expect(html).not.toContain("onerror");
  });

  it("removes style, form and button elements and style attributes", () => {
    const html = renderSafeMarkdown('<style>body{display:none}</style>\n\n<form><button>go</button></form>\n\n<p style="color:red">x</p>');
    expect(html).not.toContain("<style");
    expect(html).not.toContain("<form");
    expect(html).not.toContain("<button");
    expect(html).not.toContain("style=");
  });

  it("drops a javascript: href but keeps the link text", () => {
    const html = renderSafeMarkdown("[click](javascript:alert(1))");
    expect(html).not.toContain("javascript:");
    expect(html).toContain("click");
  });

  it("drops a remote image src and keeps an embedded data image", () => {
    const remote = renderSafeMarkdown("![x](https://evil.example/pixel.png)");
    expect(remote).not.toContain("https://evil.example");
    const embedded = renderSafeMarkdown("![x](data:image/png;base64,iVBORw0KGgo=)");
    expect(embedded).toContain("data:image/png;base64,iVBORw0KGgo=");
  });
});

describe("what the sanitiser keeps (§9.7 acceptance)", () => {
  it("keeps http(s) and mailto links and marks them noopener", () => {
    const html = renderSafeMarkdown("[docs](https://example.com/a) and [mail](mailto:dev@example.com)");
    expect(html).toContain('href="https://example.com/a"');
    expect(html).toContain('href="mailto:dev@example.com"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("keeps relative links, which RN then declines to open", () => {
    expect(renderSafeMarkdown("[plan](./docs/plan.md)")).toContain('href="./docs/plan.md"');
  });

  it("renders headings, lists, fenced code and GFM tables", () => {
    const html = renderSafeMarkdown(
      [
        "# Title",
        "## Section",
        "- one",
        "- two",
        "```rust",
        "fn main() {}",
        "```",
        "| a | b |",
        "| --- | --- |",
        "| 1 | 2 |",
      ].join("\n"),
    );
    expect(html).toContain("<h1");
    expect(html).toContain("<h2");
    expect(html).toContain("<ul>");
    expect(html).toContain("<pre>");
    expect(html).toContain("language-rust");
    expect(html).toContain("<table>");
    expect(html).toContain("<td>1</td>");
  });

  it("drops the checkbox of a task list, because the shared config forbids <input>", () => {
    // The desktop behaves identically; §9.7's acceptance note reads better than
    // the sanitiser it shares. The list item and its text survive.
    const html = renderSafeMarkdown("- [x] done\n- [ ] todo");
    expect(html).not.toContain("<input");
    expect(html).toContain("done");
    expect(html).toContain("todo");
  });
});
