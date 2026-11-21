import { beforeEach, describe, expect, it } from "vitest";

import { forgetDrafts, readDraft, writeDraft } from "./drafts";

beforeEach(forgetDrafts);

describe("drafts", () => {
  it("keeps a draft per chat", () => {
    writeDraft("server-a", "%7", "half a thought");
    writeDraft("server-a", "%8", "another one");
    expect(readDraft("server-a", "%7")).toBe("half a thought");
    expect(readDraft("server-a", "%8")).toBe("another one");
    expect(readDraft("server-a", "%9")).toBe("");
  });

  it("the same pane id on another server is another chat", () => {
    writeDraft("server-a", "%7", "for a");
    expect(readDraft("server-b", "%7")).toBe("");
    writeDraft("server-b", "%7", "for b");
    expect(readDraft("server-a", "%7")).toBe("for a");
    expect(readDraft("server-b", "%7")).toBe("for b");
  });

  it("does not mix a server and a pane whose names run together", () => {
    writeDraft("a", "b%1", "one");
    expect(readDraft("ab", "%1")).toBe("");
  });

  it("keeps the text as typed, and replaces it", () => {
    writeDraft("server-a", "%7", "  two lines\nand a space ");
    expect(readDraft("server-a", "%7")).toBe("  two lines\nand a space ");
    writeDraft("server-a", "%7", "rewritten");
    expect(readDraft("server-a", "%7")).toBe("rewritten");
  });

  it("an emptied field leaves no draft", () => {
    writeDraft("server-a", "%7", "typed");
    writeDraft("server-a", "%7", "");
    expect(readDraft("server-a", "%7")).toBe("");
    writeDraft("server-a", "%7", "typed");
    writeDraft("server-a", "%7", " \n ");
    expect(readDraft("server-a", "%7")).toBe("");
  });

  it("keeps nothing before the server is known", () => {
    writeDraft("", "%7", "typed too early");
    expect(readDraft("", "%7")).toBe("");
    expect(readDraft("server-a", "%7")).toBe("");
  });
});
