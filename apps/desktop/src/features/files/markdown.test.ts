// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { renderSafeMarkdown, renderSafeSvg } from "./markdown";

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

  it("sanitizes active and remote content from SVG image previews", () => {
    const svg = renderSafeSvg(`<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(2)</script><image href="https://tracker.invalid/a"/><path d="M0 0"/></svg>`);
    expect(svg).toContain("<svg");
    expect(svg).toContain("<path");
    expect(svg).not.toMatch(/script|onload|https:/i);
  });
});
