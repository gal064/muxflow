// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { markdownFilePath, renderSafeMarkdown, renderSafeSvg } from "./markdown";

describe("renderSafeMarkdown", () => {
  it("renders ordinary Markdown while removing scriptable markup and URLs", () => {
    const html = renderSafeMarkdown(`# Safe\n\n[good](https://example.com) [bad](javascript:alert(1))\n\n<img src=x onerror=alert(2)>\n<script>alert(3)</script>`);
    expect(html).toContain("<h1>Safe</h1>");
    expect(html).toContain('href="https://example.com"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).not.toMatch(/javascript:|onerror|<script/i);
  });

  it("allows relative links without granting active embedded content", () => {
    const html = renderSafeMarkdown(`[local](./guide.md)\n\n![beacon](https://tracker.invalid/pixel.png)\n\n<iframe src=https://example.com></iframe><form><input></form>`);
    expect(html).toContain('href="./guide.md"');
    expect(html).not.toMatch(/iframe|form|input|tracker\.invalid/i);
  });

  it("keeps TODO-style bare paths while refusing schemes and network references", () => {
    const html = renderSafeMarkdown("[task](todos/T003-pricing-usage-limits.md) [back](../todo.md) [bad](file:///etc/passwd) [network](//example.com/doc) [data](data:text/html,hello)");
    expect(html).toContain('href="todos/T003-pricing-usage-limits.md"');
    expect(html).toContain('href="../todo.md"');
    expect(html).not.toMatch(/href="(?:file:|\/\/|data:)/);
  });

  it("sanitizes active and remote content from SVG image previews", () => {
    const svg = renderSafeSvg(`<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(2)</script><image href="https://tracker.invalid/a"/><path d="M0 0"/></svg>`);
    expect(svg).toContain("<svg");
    expect(svg).toContain("<path");
    expect(svg).not.toMatch(/script|onload|https:/i);
  });
});

describe("markdownFilePath", () => {
  it("resolves TODO links against the document rather than the pane cwd", () => {
    expect(markdownFilePath("/repo/todo.md", "todos/T003-pricing-usage-limits.md")).toBe("/repo/todos/T003-pricing-usage-limits.md");
    expect(markdownFilePath("/repo/todos/T003.md", "../docs/03-pre-launch.md")).toBe("/repo/todos/../docs/03-pre-launch.md");
    expect(markdownFilePath("/repo/docs/plan.md", "./other.md")).toBe("/repo/docs/./other.md");
    expect(markdownFilePath("/repo/docs/plan.md", "/repo/todo.md")).toBe("/repo/todo.md");
  });

  it("decodes filenames and separates URL fragments and queries", () => {
    expect(markdownFilePath("/repo/todo.md", "my%20plan%23one.md#section")).toBe("/repo/my plan#one.md");
    expect(markdownFilePath("/repo/todo.md", "plan.md?raw=1")).toBe("/repo/plan.md");
    expect(markdownFilePath("/repo/todo.md", "plan.md#section")).toBe("/repo/plan.md");
  });

  it("leaves external and same-document links alone and rejects malformed paths", () => {
    for (const href of ["https://example.com", "mailto:me@example.com", "#section", "//example.com/file", "javascript:alert(1)", "file:///etc/passwd"]) {
      expect(markdownFilePath("/repo/todo.md", href)).toBeUndefined();
    }
    expect(() => markdownFilePath("/repo/todo.md", "bad%00.md")).toThrow();
    expect(() => markdownFilePath("/repo/todo.md", "bad%XX.md")).toThrow();
  });
});
