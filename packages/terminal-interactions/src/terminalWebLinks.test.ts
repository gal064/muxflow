import { describe, expect, it } from "vitest";
import { terminalWebLinks } from "./terminalWebLinks";

describe("terminal web links", () => {
  it("leaves closing prose punctuation outside the URL range", () => {
    const line = "Read (https://nathanbarry.com/2016-review/): next";
    const start = line.indexOf("https://");
    const text = "https://nathanbarry.com/2016-review/";

    expect(terminalWebLinks(line)).toEqual([{ text, start, end: start + text.length }]);
  });

  it.each([".", ",", ";", ":", "!", "?", "`", ").", "]:", "}!"])(
    "trims the trailing prose suffix %s",
    (suffix) => expect(terminalWebLinks(`https://example.com/path${suffix}`)[0]?.text).toBe("https://example.com/path"),
  );

  it("preserves balanced delimiters and punctuation inside a URL", () => {
    expect(terminalWebLinks([
      "https://example.com/foo_(bar)",
      "https://example.com/a:b",
      "https://example.com/search?q=x:y",
      "http://[::1]:3000/path",
    ].join(" ")).map((link) => link.text)).toEqual([
      "https://example.com/foo_(bar)",
      "https://example.com/a:b",
      "https://example.com/search?q=x:y",
      "http://[::1]:3000/path",
    ]);
  });

  it("trims only excess closing delimiters", () => {
    expect(terminalWebLinks("https://example.com/foo_(bar))]:")[0]?.text).toBe("https://example.com/foo_(bar)");
  });

  it("finds multiple links with their exact offsets", () => {
    const line = "One https://one.example/a, then https://two.example/b.";
    expect(terminalWebLinks(line)).toEqual([
      { text: "https://one.example/a", start: 4, end: 25 },
      { text: "https://two.example/b", start: 32, end: 53 },
    ]);
    expect(terminalWebLinks("no web link here")).toEqual([]);
  });
});
