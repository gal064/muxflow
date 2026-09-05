import { describe, expect, it } from "vitest";

import { externalLinkTarget, markdownNavigationStaysInApp } from "./externalLinks";

describe("external link policy", () => {
  it("hands web and mail links to the device and rejects embedded or repository navigation", () => {
    expect(externalLinkTarget(" https://example.com/docs ")).toBe("https://example.com/docs");
    expect(externalLinkTarget("http://example.com")).toBe("http://example.com");
    expect(externalLinkTarget("mailto:hello@example.com")).toBe("mailto:hello@example.com");
    expect(externalLinkTarget("javascript:alert(1)")).toBeUndefined();
    expect(externalLinkTarget("https://example.com\njavascript:alert(1)")).toBeUndefined();
    expect(externalLinkTarget("./README.md")).toBeUndefined();
  });

  it("allows only the bundled about page to navigate inside the Markdown WebView", () => {
    expect(markdownNavigationStaysInApp("about:blank")).toBe(true);
    expect(markdownNavigationStaysInApp("about:srcdoc")).toBe(true);
    expect(markdownNavigationStaysInApp("https://example.com")).toBe(false);
  });
});
