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

  it("keeps explicit column and row header semantics for AT-SPI", () => {
    const parsed = new DOMParser().parseFromString(renderSafeMarkdown("| Name | Value |\n| --- | --- |\n| One | Two |"), "text/html");
    expect([...parsed.querySelectorAll("th")].map((cell) => cell.getAttribute("role"))).toEqual(["columnheader", "columnheader"]);
    const row = new DOMParser().parseFromString(renderSafeMarkdown('<table><tr><th scope="row">Name</th><td>Value</td></tr></table>'), "text/html");
    expect(row.querySelector("th")?.getAttribute("role")).toBe("rowheader");
    const explicit = new DOMParser().parseFromString(renderSafeMarkdown('<table><tr><th role="rowheader">Name</th><td>Value</td></tr></table>'), "text/html");
    expect(explicit.querySelector("th")?.getAttribute("role")).toBe("rowheader");
    const implicit = new DOMParser().parseFromString(renderSafeMarkdown('<table><tr><th>Name</th><th>Value</th></tr><tr><th>One</th><td>Two</td></tr></table>'), "text/html");
    expect([...implicit.querySelectorAll("th")].map((cell) => cell.getAttribute("role"))).toEqual(["columnheader", "columnheader", "rowheader"]);
  });

  it("accounts for rowspans and automatic scope when making header roles explicit", () => {
    const html = renderSafeMarkdown('<table><tr><th scope="auto">A</th><th>B</th></tr><tr><td rowspan="2">Group</td><td>Value</td></tr><tr><th>Detail</th></tr></table>');
    const parsed = new DOMParser().parseFromString(html, "text/html");
    expect([...parsed.querySelectorAll("th")].map((cell) => cell.getAttribute("role"))).toEqual(["columnheader", "columnheader", "cell"]);
    const groups = renderSafeMarkdown('<table><tbody><tr><td rowspan="0">Group</td><td>Value</td></tr><tr><th>Detail</th></tr></tbody><tbody><tr><th>Next group</th><td>Value</td></tr></tbody></table>');
    const grouped = new DOMParser().parseFromString(groups, "text/html");
    expect([...grouped.querySelectorAll("th")].map((cell) => cell.getAttribute("role"))).toEqual(["cell", "rowheader"]);
  });

  it("rejects the balanced-parenthesis CSS image payload in ordinary Markdown", () => {
    const html = renderSafeMarkdown('![x](data:image/png;base64,AAAA("),url("https://example.invalid/x")/*)');
    const image = new DOMParser().parseFromString(html, "text/html").querySelector("img");
    expect(image?.hasAttribute("src")).toBe(false);
  });

  it("rejects quote and backslash payloads after an embedded image prefix", () => {
    for (const payload of ['AAAA"),url("https://tracker.invalid/pixel.png', "AAAA\\evil", "AAAA&#34;),url(https://tracker.invalid/pixel.png)"]) {
      const html = renderSafeMarkdown(`<img src='data:image/png;base64,${payload}'>`);
      const image = new DOMParser().parseFromString(html, "text/html").querySelector("img");
      expect(image?.hasAttribute("src")).toBe(false);
    }
    expect(renderSafeMarkdown("![valid](data:image/png;base64,AAAA)")).toContain('src="data:image/png;base64,AAAA"');
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
