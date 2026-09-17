import { describe, expect, it } from "vitest";
import type { FileBody } from "./fileStream";
import { decodeText, filePresentation } from "./presentation";

const encoder = new TextEncoder();

function text(source: string): FileBody {
  return { kind: "text", bytes: encoder.encode(source), metadata: undefined };
}

describe("mode selection (§9.7 step 1)", () => {
  it("renders Markdown for .md and .markdown, whatever the case", () => {
    for (const name of ["PLAN.md", "plan.MD", "notes.markdown"]) {
      expect(filePresentation(text("# hi"), name)).toEqual({ kind: "markdown", text: "# hi" });
    }
  });

  it("shows every other text file plain, including Markdown-looking ones", () => {
    for (const name of ["main.rs", "readme.mdx", "Makefile", "notes.md.bak"]) {
      expect(filePresentation(text("body"), name).kind).toBe("plain");
    }
  });

  it("never renders Markdown for a classification that carried no text", () => {
    expect(filePresentation({ kind: "binary" }, "README.md")).toEqual({
      kind: "placeholder",
      message: "This is a binary file.",
    });
  });
});

describe("placeholders (§9.7 step 1)", () => {
  it("spells each of the four strings the document gives", () => {
    expect(filePresentation({ kind: "tooLarge", size: 3n * 1024n * 1024n }, "big.log")).toEqual({
      kind: "placeholder",
      message: "This file is too large to show here (3.0 MB).",
    });
    expect(filePresentation({ kind: "binary" }, "a.bin").kind).toBe("placeholder");
    expect(filePresentation({ kind: "image" }, "a.png")).toEqual({
      kind: "placeholder",
      message: "Images aren't shown in this version.",
    });
    expect(filePresentation({ kind: "unavailable" }, "a.txt")).toEqual({
      kind: "placeholder",
      message: "Couldn't open this file.",
    });
  });
});

describe("decoding", () => {
  it("replaces invalid UTF-8 rather than throwing", () => {
    expect(decodeText(new Uint8Array([0x61, 0xff, 0x62]))).toBe("a�b");
  });
});
