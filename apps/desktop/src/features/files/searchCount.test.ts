// @vitest-environment jsdom
// jsdom, because the label is placed into a real search panel.
import { closeSearchPanel, findNext, openSearchPanel, search, SearchQuery, setSearchQuery } from "@codemirror/search";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { SEARCH_COUNT_LIMIT, searchCount, searchCountLabel } from "./searchCount";

const nextFrame = () => new Promise((resolve) => requestAnimationFrame(resolve));

function searched(doc: string, query: string, selection?: { anchor: number; head: number }) {
  const state = EditorState.create({ doc, selection, extensions: [search()] });
  return state.update({ effects: setSearchQuery.of(new SearchQuery({ search: query })) }).state;
}

describe("searchCountLabel", () => {
  it("says which match the selection is on, out of how many", () => {
    expect(searchCountLabel(searched("a cat, a cat, a cat", "cat", { anchor: 9, head: 12 }))).toBe("2 of 3");
  });

  it("does not know which match when the selection is on none", () => {
    expect(searchCountLabel(searched("a cat, a cat", "cat"))).toBe("? of 2");
  });

  it("says when there is nothing to find, and nothing without a query", () => {
    expect(searchCountLabel(searched("a cat", "dog"))).toBe("No results");
    expect(searchCountLabel(searched("a cat", ""))).toBe("");
  });

  it("stops counting at the limit", () => {
    const doc = "x".repeat(SEARCH_COUNT_LIMIT + 5);
    expect(searchCountLabel(searched(doc, "x", { anchor: 0, head: 1 }))).toBe(`1 of ${SEARCH_COUNT_LIMIT}+`);
    expect(searchCountLabel(searched(doc, "x", { anchor: doc.length - 1, head: doc.length }))).toBe(`? of ${SEARCH_COUNT_LIMIT}+`);
  });
});

describe("searchCount", () => {
  let view: EditorView | undefined;
  // jsdom lays nothing out; CodeMirror's measure pass still asks for text rects.
  beforeAll(() => {
    Range.prototype.getClientRects = () => ({ length: 0, item: () => null, [Symbol.iterator]: [][Symbol.iterator] }) as unknown as DOMRectList;
    Range.prototype.getBoundingClientRect = () => new DOMRect();
  });
  afterEach(() => {
    view?.destroy();
    view = undefined;
  });

  it("shows the count in the search panel and follows the selection", async () => {
    view = new EditorView({ parent: document.body, doc: "one two one two", extensions: [search(), searchCount] });
    openSearchPanel(view);
    view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "two" })) });
    findNext(view);
    // The label is placed in CodeMirror's next measure frame.
    await nextFrame();
    const label = view.dom.querySelector(".cm-search .cm-search-count");
    expect(label?.previousElementSibling?.getAttribute("name")).toBe("select");
    expect(label?.textContent).toBe("1 of 2");
    findNext(view);
    await nextFrame();
    expect(label?.textContent).toBe("2 of 2");
    view.dispatch({ changes: { from: 0, insert: "two " } });
    await nextFrame();
    expect(label?.textContent).toBe("3 of 3");
  });

  it("counts edits made while the panel was closed when it opens again", async () => {
    view = new EditorView({ parent: document.body, doc: "one two", extensions: [search(), searchCount] });
    openSearchPanel(view);
    view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: "two" })) });
    await nextFrame();
    expect(view.dom.querySelector(".cm-search-count")?.textContent).toBe("? of 1");
    closeSearchPanel(view);
    view.dispatch({ changes: { from: 0, insert: "two " } });
    openSearchPanel(view);
    await nextFrame();
    expect(view.dom.querySelector(".cm-search .cm-search-count")?.textContent).toBe("? of 2");
  });
});
