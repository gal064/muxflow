import { describe, expect, it } from "vitest";
import { isExplicitTerminalFilePath, resolveTerminalFilePath, terminalFileLinkCellRange, terminalFileLinks } from "./terminalFilePaths";

describe("terminal file path links", () => {
  it("finds absolute, home-relative, and separator-bearing relative paths without swallowing prose wrappers", () => {
    expect(terminalFileLinks("See (/tmp/demo.ts), ~/dev/report.pdf, then src/main.ts and ../README.md.")).toEqual([
      { text: "/tmp/demo.ts", start: 5, end: 17 },
      { text: "~/dev/report.pdf", start: 20, end: 36 },
      { text: "src/main.ts", start: 43, end: 54 },
      { text: "../README.md", start: 59, end: 71 },
    ]);
  });

  it.each(['"', "'"])("links only the path inside a %s quoted HTML attribute", (quote) => {
    const path = "docs/assets/feature-paste.png";
    const line = `<img src=${quote}${path}${quote} alt=${quote}Screenshot${quote}>`;
    const start = line.indexOf(path);

    expect(terminalFileLinks(line)).toEqual([{ text: path, start, end: start + path.length }]);
  });

  it("preserves balanced delimiters inside paths while trimming prose wrappers", () => {
    expect(terminalFileLinks("See assets/image_(dark), (docs/[final]), and {build/{release}}.")
      .map((link) => link.text)).toEqual([
      "assets/image_(dark)",
      "docs/[final]",
      "build/{release}",
    ]);
  });

  it("leaves a closing parenthesis and following colon outside the file link", () => {
    const line = "Correct Q01 baseline (tmp/report-slim-v2-qa/q01-baseline-corrected/viewport.png): Failed 1";
    const text = "tmp/report-slim-v2-qa/q01-baseline-corrected/viewport.png";
    const start = line.indexOf(text);

    expect(terminalFileLinks(line)).toEqual([{ text, start, end: start + text.length }]);
  });

  it.each([":1", ":12:4"])("recognizes a file reference while leaving %s outside the link", (location) => {
    const line = `Updated (sampleco-projectx/meetings/DECK-PLAN.md${location}) to match the current deck`;
    const text = "sampleco-projectx/meetings/DECK-PLAN.md";
    const start = line.indexOf(text);

    expect(terminalFileLinks(line)).toEqual([{ text, start, end: start + text.length }]);
  });

  it.each([
    "README.md", "https://example.com/a.ts", "file:///tmp/a", "mailto:foo/bar",
    "www.example.com/path", "example.com/path", "user@host:path/to",
    "~alice/private.txt", "~//tmp/file", "~/",
  ])(
    "does not link the out-of-scope token %s",
    (value) => expect(isExplicitTerminalFilePath(value)).toBe(false),
  );

  it("resolves relative paths lexically from a POSIX pane cwd", () => {
    expect(resolveTerminalFilePath("./src/../README.md", "/home/dev/project")).toBe("/home/dev/project/README.md");
    expect(resolveTerminalFilePath("../../etc/hosts", "/home/dev")).toBe("/etc/hosts");
    expect(resolveTerminalFilePath("/tmp/file", "/ignored")).toBe("/tmp/file");
  });

  it("resolves current-user home paths only with an authoritative absolute home", () => {
    expect(resolveTerminalFilePath("~/dev/../report.pdf", "/ignored", "/home/dev")).toBe("/home/dev/report.pdf");
    expect(resolveTerminalFilePath("~/report.pdf", "/ignored")).toBeUndefined();
    expect(resolveTerminalFilePath("~/report.pdf", "/ignored", "relative/home")).toBeUndefined();
  });

  it("maps UTF-16 token offsets to inclusive xterm cells", () => {
    const line = bufferLine(["界", "", " ", "e\u0301", " ", "/", "t", "m", "p", "/", "a"], [2, 0]);
    expect(terminalFileLinkCellRange(line, 5, 11)).toEqual({ start: 5, end: 10 });
  });
});

function bufferLine(chars: string[], widths: number[] = []) {
  return {
    length: chars.length,
    getCell: (index: number) => ({
      getChars: () => chars[index] ?? "",
      getWidth: () => widths[index] ?? 1,
    }),
  };
}
