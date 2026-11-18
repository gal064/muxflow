import { describe, expect, it } from "vitest";

import { plainText } from "./plainText";

describe("a reply as plain text", () => {
  it("drops the marks around words and keeps what they say", () => {
    expect(plainText("The **fix** is in `links.rs`, *not* pushed. See [the docs](https://example.com/docs)."))
      .toBe("The fix is in links.rs, not pushed. See the docs.");
  });

  it("keeps paragraphs and headings apart", () => {
    expect(plainText("# What changed\n\nOne line.\n\nAnother.")).toBe("What changed\n\nOne line.\n\nAnother.");
  });

  it("keeps code as written, without its fence", () => {
    expect(plainText("Run:\n\n```sh\npnpm test\n  --filter mobile\n```\n\nDone.")).toBe("Run:\n\npnpm test\n  --filter mobile\n\nDone.");
  });

  it("writes a list with its markers, a list inside it moved in", () => {
    expect(plainText("- one\n- two\n  1. first\n  2. second\n- three")).toBe("• one\n• two\n  1. first\n  2. second\n• three");
    expect(plainText("3. third\n4. fourth")).toBe("3. third\n4. fourth");
    expect(plainText("- [x] done\n- [ ] not yet")).toBe("[x] done\n[ ] not yet");
  });

  it("writes a table as rows with a tab between cells", () => {
    expect(plainText("| File | Lines |\n| --- | --- |\n| `links.rs` | 3 |\n| open.rs | 1 |")).toBe("File\tLines\nlinks.rs\t3\nopen.rs\t1");
  });

  it("keeps a quote's words and leaves a rule out", () => {
    expect(plainText("> quoted *words*\n\n---\n\nAfter.")).toBe("quoted words\n\nAfter.");
  });

  it("is the text itself when there is no Markdown in it", () => {
    expect(plainText("Not pushed. The remote branch is unchanged.")).toBe("Not pushed. The remote branch is unchanged.");
    expect(plainText("")).toBe("");
  });
});
