// @vitest-environment jsdom
// jsdom, because the commands run against a real EditorView.
import { Compartment, EditorSelection, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cursorCharLeft, deleteCharBackward, history, redo, undo } from "@codemirror/commands";
import { blockMarkupEnd, breakBeforeHeading, changeListIndent, continueMarkup, linkAt, linkTarget, liveMarkdown, liveRanges, setFocused, softBreaks, tableRanges, toggleInline, type LiveRange } from "./liveMarkdown";

function state(doc: string, cursor?: number, focused = cursor !== undefined) {
  const created = EditorState.create({
    doc,
    selection: cursor === undefined ? undefined : EditorSelection.cursor(cursor),
    extensions: liveMarkdown({ onLink: () => undefined }),
  });
  return created.update({ effects: setFocused.of(focused) }).state;
}

function ranges(doc: string, cursor?: number): LiveRange[] {
  const current = state(doc, cursor);
  return liveRanges(current, 0, current.doc.length);
}

function hidden(doc: string, cursor?: number): string[] {
  return ranges(doc, cursor)
    .filter((range) => range.kind === "hide")
    .map((range) => doc.slice(range.from, range.to));
}

const views: EditorView[] = [];
afterEach(() => { for (const view of views.splice(0)) view.destroy(); });

function view(doc: string, cursor: number | { anchor: number; head: number }) {
  const selection = typeof cursor === "number" ? EditorSelection.cursor(cursor) : EditorSelection.range(cursor.anchor, cursor.head);
  const created = new EditorView({
    state: EditorState.create({ doc, selection, extensions: liveMarkdown({ onLink: () => undefined }) }),
    parent: document.body,
  });
  views.push(created);
  return created;
}

describe("live Markdown rendering", () => {
  it("hides heading, emphasis and code markers away from the caret", () => {
    const doc = "# Title\n\nSome **bold** and *it* and `code`.\n";
    expect(hidden(doc)).toEqual(["# ", "**", "**", "*", "*", "`", "`"]);
    expect(ranges(doc)).toContainEqual({ kind: "line", from: 0, className: "cm-live-h1" });
  });

  it("shows the markers of the inline element the caret is in, and only that element", () => {
    const doc = "# Title\n\nSome **bold** and `code`.\n";
    expect(hidden(doc, 11)).toEqual(["# ", "**", "**", "`", "`"]);
    expect(hidden(doc, 17)).toEqual(["# ", "`", "`"]);
    // At the edge counts as in it: the caret just after `**` is where typing continues the bold.
    expect(hidden(doc, 22)).toEqual(["# ", "`", "`"]);
    expect(hidden(doc, 23)).toEqual(["# ", "**", "**", "`", "`"]);
    expect(hidden(doc, 30)).toEqual(["# ", "**", "**"]);
  });

  it("reveals active headings while keeping other block markers whole", () => {
    const doc = "## Title\n\n> - [ ] task\n\n---\n";
    expect(hidden(doc, 5)).toEqual(["> ", "- "]);
    for (const cursor of [18, doc.length - 2]) expect(hidden(doc, cursor)).toEqual(["## ", "> ", "- "]);
    const atomic = ranges(doc, 5).filter((range) => range.kind === "atomic").map((range) => doc.slice(range.from, range.to));
    expect(atomic).toEqual(["> ", "- ", "[ ] ", "---"]);
  });

  it("keeps a nascent horizontal rule editable while entering bold on an empty line", () => {
    const editor = view("Unicode – 😀\n\n", 14);
    editor.dispatch({ effects: setFocused.of(true) });
    toggleInline("**")(editor);
    expect(editor.state.doc.toString()).toBe("Unicode – 😀\n\n****");
    expect(editor.contentDOM.querySelector(".cm-live-rule")).toBeNull();
    editor.dispatch(editor.state.replaceSelection("bold"));
    expect(editor.state.doc.toString()).toBe("Unicode – 😀\n\n**bold**");
    editor.dispatch({ effects: setFocused.of(false) });
    const inactive = state("---\n\nend");
    expect(liveRanges(inactive, 0, inactive.doc.length)).toContainEqual({ kind: "rule", from: 0, to: 3 });
  });

  it.each([1, 2, 3, 4, 5, 6])("reveals level %i heading markers at the beginning, text and end", (level) => {
    const mark = "#".repeat(level);
    const doc = `${mark} Title ${mark}\n\nbody`;
    expect(hidden(doc)).toEqual([`${mark} `, ` ${mark}`]);
    for (const cursor of [0, level + 1, doc.indexOf("\n")]) {
      expect(hidden(doc, cursor)).toEqual([]);
      expect(ranges(doc, cursor).filter((range) => range.kind === "atomic")).toEqual([]);
    }
    expect(hidden(doc, doc.length)).toEqual([`${mark} `, ` ${mark}`]);
  });

  it("reveals a heading on focus even when the initial cursor has not moved", () => {
    const editor = view("### Title\n\nbody", 0);
    expect(editor.contentDOM.textContent).toBe("Titlebody");
    editor.dispatch({ effects: setFocused.of(true) });
    expect(editor.contentDOM.textContent).toBe("### Titlebody");
    expect(editor.state.selection.main.head).toBe(0);
    editor.dispatch({ effects: setFocused.of(false) });
    expect(editor.contentDOM.textContent).toBe("Titlebody");
    expect(editor.state.doc.toString()).toBe("### Title\n\nbody");
  });

  it.each([
    "- parent\n  - item  \n    continued\n\n    another paragraph",
    "- parent\n  - [ ] item\n    continued",
    "1. parent\n   2. item\\\n      continued",
  ])("aligns visible continuation lines with their nested item's content: %s", (doc) => {
    const editor = view(doc, doc.length);
    const continuations = editor.contentDOM.querySelectorAll<HTMLElement>(".cm-live-list-continuation");
    expect(continuations.length).toBeGreaterThan(0);
    for (const line of continuations) {
      expect(line.style.getPropertyValue("--cm-list-depth")).toBe("1");
      expect(line.textContent).not.toMatch(/^\s/);
    }
  });

  it("renders everything while the editor does not have focus", () => {
    const doc = "# Title\n";
    expect(hidden(doc, undefined)).toEqual(["# "]);
    const unfocused = state(doc, 2, false);
    expect(liveRanges(unfocused, 0, doc.length).filter((range) => range.kind === "hide")).toHaveLength(1);
  });

  it("draws a link as its text and keeps the target out of sight", () => {
    const doc = "See [the docs](./docs/a.md \"title\") now.\n\nnext";
    expect(hidden(doc, doc.length)).toEqual(["[", "](./docs/a.md \"title\")"]);
    expect(ranges(doc, doc.length)).toContainEqual({ kind: "mark", from: 5, to: 13, className: "cm-live-link" });
  });

  it("draws a full reference link as a link", () => {
    const doc = "See [the docs][ref].\n\n[ref]: ./a.md";
    expect(ranges(doc)).toContainEqual({ kind: "mark", from: 5, to: 13, className: "cm-live-link" });
  });

  it("leaves a bracketed word without a destination as text, keeping its emphasis", () => {
    const doc = "Marks: **[beta]** means it.";
    expect(ranges(doc).some((range) => range.kind === "mark" && range.className === "cm-live-link")).toBe(false);
    expect(hidden(doc)).toEqual(["**", "**"]);
  });

  it("renders only embedded raster images, as the preview does", () => {
    const embedded = "![dot](data:image/png;base64,AAAA)\n\nx";
    expect(ranges(embedded, embedded.length)).toContainEqual({ kind: "image", from: 0, to: 34, src: "data:image/png;base64,AAAA", alt: "dot" });
    const remote = "![logo](https://example.com/a.png)\n\nx";
    expect(ranges(remote, remote.length).some((range) => range.kind === "image")).toBe(false);
    expect(hidden(remote, remote.length)).toEqual(["![", "](https://example.com/a.png)"]);
  });

  it("draws bullets and tasks, and leaves ordered numbers alone", () => {
    const doc = "- one\n- [ ] open\n- [x] done\n1. first\n";
    const drawn = ranges(doc);
    expect(drawn).toContainEqual({ kind: "bullet", from: 0, to: 2 });
    expect(drawn).toContainEqual({ kind: "task", from: 8, to: 12, checked: false });
    expect(drawn).toContainEqual({ kind: "task", from: 19, to: 23, checked: true });
    expect(hidden(doc)).toEqual(["- ", "- "]);
  });

  it("includes task separator spaces in the checkbox's layout column", () => {
    const doc = "- [ ]   task  \n  continued";
    const editor = view(doc, doc.length);
    expect(editor.contentDOM.querySelector(".cm-live-list")?.textContent).toBe("task  ");
    expect(editor.contentDOM.querySelector(".cm-live-list-continuation")?.textContent).toBe("continued");
    expect(editor.state.doc.toString()).toBe(doc);
  });

  it("hides fences but keeps code lines styled and the language visible", () => {
    const doc = "```ts\nconst a = 1;\n```\n\ntext";
    expect(hidden(doc, doc.length)).toEqual(["```", "```"]);
    expect(ranges(doc, doc.length).filter((range) => range.kind === "line").map((range) => range.className))
      .toEqual(["cm-live-code cm-live-code-first", "cm-live-code", "cm-live-code cm-live-code-last"]);
    expect(hidden(doc, 8)).toEqual([]);
  });

  it("hides quote markers and escapes", () => {
    expect(hidden("> quoted \\*text\\*\n")).toEqual(["> ", "\\", "\\"]);
  });

  it("renders a table whole wherever the caret is", () => {
    const doc = "| a | b |\n| - | - |\n| 1 | 2 |\n\nafter";
    expect(tableRanges(state(doc, doc.length))).toEqual([{ from: 0, to: 29, source: doc.slice(0, 29) }]);
    expect(tableRanges(state(doc, 12))).toEqual([{ from: 0, to: 29, source: doc.slice(0, 29) }]);
  });

  it("renders an indented table, as the preview does", () => {
    const doc = "x\n\n  a | b\n  --|--\n  1 | 2";
    expect(tableRanges(state(doc)).map((table) => table.from)).toEqual([3]);
  });

  it("finds the soft line breaks a paragraph flows across, and only those", () => {
    const joined = (doc: string) => softBreaks(state(doc)).map((range) => JSON.stringify(doc.slice(range.from, range.to)));
    expect(joined("one\ntwo\nthree\n\nnext\npara")).toEqual(['"\\n"', '"\\n"', '"\\n"']);
    // A list item's continuation takes its indentation with the break.
    expect(joined("- item that\n  wraps")).toEqual(['"\\n  "']);
    // Hard breaks, separate blocks and code stay as they are.
    expect(joined("hard  \nbreak\\\nhere")).toEqual([]);
    expect(joined("# Title\ntext\n```\ncode\nmore\n```")).toEqual([]);
    expect(joined("> quoted\n> on two")).toEqual(['"\\n"']);
    // A single trailing space goes with the break; an escaped backslash is text, not a hard break.
    expect(joined("one \ntwo")).toEqual(['" \\n"']);
    expect(joined("one \\\\\ntwo")).toEqual(['"\\n"']);
  });

  it("drops the joins of a paragraph that an edit below turns into a table", () => {
    const editor = view("x\ny\na | b\n\nend", 0);
    const at = "x\ny\na | b".length;
    editor.dispatch({ changes: { from: at, insert: "\n-|-" } });
    expect(editor.contentDOM.querySelectorAll(".cm-live-table").length).toBe(1);
    expect([...editor.contentDOM.querySelectorAll(".cm-line")][0].textContent).toBe("x y");
  });

  it("finds where a line's text starts after its hidden markup", () => {
    const at = (doc: string, line = 1) => { const current = state(doc); return blockMarkupEnd(current, current.doc.line(line)) - current.doc.line(line).from; };
    expect(at("## Title")).toBe(3);
    expect(at("> - [x] done")).toBe(8);
    expect(at("- a\n  - nested", 2)).toBe(4);
    expect(at("1. first")).toBe(0);
    expect(at("plain")).toBe(0);
    expect(at("Title\n===", 1)).toBe(0);
  });

  it("finds the target of a link, an autolink and a bare URL", () => {
    const doc = "[a](x.md) <https://a.dev> https://b.dev ![i](y.png)";
    const current = state(doc);
    expect(linkAt(current, 1)).toBe("x.md");
    expect(linkAt(current, 13)).toBe("https://a.dev");
    expect(linkAt(current, 30)).toBe("https://b.dev");
    expect(linkAt(current, 43)).toBeUndefined();
  });
});

describe("linkTarget", () => {
  it("resolves a destination as the preview does", () => {
    expect(linkTarget("<x y.md>")).toBe("x y.md");
    expect(linkTarget("<https://x.y>")).toBe("https://x.y");
    expect(linkTarget("a\\(b\\).md")).toBe("a(b).md");
    expect(linkTarget("www.example.com")).toBe("http://www.example.com");
    expect(linkTarget("./plain.md")).toBe("./plain.md");
    expect(linkTarget("me@example.com")).toBe("mailto:me@example.com");
  });
});

describe("live Markdown commands", () => {
  it("moves and edits the caret through a revealed heading prefix", () => {
    const editor = view("### Title", 4);
    editor.dispatch({ effects: setFocused.of(true) });
    cursorCharLeft(editor);
    expect(editor.state.selection.main.head).toBe(3);
    editor.dispatch({ selection: { anchor: 0 } });
    expect(editor.state.selection.main.head).toBe(0);
    editor.dispatch({ selection: { anchor: 3 } });
    deleteCharBackward(editor);
    expect(editor.state.doc.toString()).toBe("## Title");
    editor.dispatch({ selection: { anchor: 3 } });
    expect(breakBeforeHeading(editor)).toBe(true);
    expect(editor.state.doc.toString()).toBe("\n## Title");
  });

  it.each([
    ["- parent\n- item\n- last", "- parent\n  - item\n- last"],
    ["1. parent\n2. item\n3. last", "1. parent\n   2. item\n3. last"],
    ["9. first\n10. parent\n11. item", "9. first\n10. parent\n    11. item"],
    ["- [x] parent\n- [ ] item", "- [x] parent\n  - [ ] item"],
    ["- parent\n1. item", "- parent\n1. item"],
    ["> - parent\n> - item", "> - parent\n>   - item"],
    ["-     parent\n- item", "-     parent\n  - item"],
    ["10. parent\n11. `item`", "10. parent\n    11. `item`"],
    ["- root\n  1.\tparent\n  2. item", "- root\n  1.\tparent\n        2. item"],
    ["- parent\n- item\n  > quote", "- parent\n  - item\n    > quote"],
    ["- parent\n- item\n  ```\n  > code\n  ```", "- parent\n  - item\n    ```\n    > code\n    ```"],
    ["> - parent\n> - item\n>   > quote", "> - parent\n>   - item\n>     > quote"],
  ])("indents whole items using the preceding marker's content column: %s", (doc, indented) => {
    const editor = view(doc, doc.indexOf("item"));
    expect(changeListIndent(editor, 1)).toBe(true);
    expect(editor.state.doc.toString()).toBe(indented);
    if (indented !== doc) {
      expect(editor.contentDOM.querySelector<HTMLElement>('.cm-live-list[style*="--cm-list-depth: 1"]')).not.toBeNull();
      expect(changeListIndent(editor, -1)).toBe(true);
      expect(editor.state.doc.toString()).toBe(doc);
    }
  });

  it("moves continuation lines and children once when their parent is selected", () => {
    const doc = "- parent\n- item\n  continued\n  - child\n- last";
    const editor = view(doc, { anchor: doc.indexOf("- item"), head: doc.indexOf("- last") });
    changeListIndent(editor, 1);
    expect(editor.state.doc.toString()).toBe("- parent\n  - item\n    continued\n    - child\n- last");
    changeListIndent(editor, -1);
    expect(editor.state.doc.toString()).toBe(doc);
  });

  it("moves tab-indented items by columns while retaining their child structure", () => {
    const doc = "- root\n\t- parent\n\t- item\n\t  - child";
    const editor = view(doc, doc.indexOf("item"));
    changeListIndent(editor, 1);
    expect(editor.state.doc.toString()).toBe("- root\n\t- parent\n      - item\n        - child");
    expect(editor.contentDOM.querySelectorAll('.cm-live-list[style*="--cm-list-depth: 2"]')).toHaveLength(1);
    changeListIndent(editor, -1);
    expect(editor.state.doc.toString()).toBe("- root\n\t- parent\n    - item\n      - child");
  });

  it("indents selected siblings as one group and restores them together", () => {
    const doc = "1. parent\n2. first\n3. second\n4. last";
    const editor = view(doc, { anchor: doc.indexOf("2."), head: doc.indexOf("4.") });
    changeListIndent(editor, 1);
    expect(editor.state.doc.toString()).toBe("1. parent\n   2. first\n   3. second\n4. last");
    changeListIndent(editor, -1);
    expect(editor.state.doc.toString()).toBe(doc);
  });

  it("keeps sibling selection structural across blank separators in a loose list", () => {
    const doc = "1. parent\n\n2. first\n\n3. second\n\n4. last";
    const editor = view(doc, { anchor: doc.indexOf("2."), head: doc.indexOf("4.") });
    changeListIndent(editor, 1);
    expect(editor.state.doc.toString()).toBe("1. parent\n\n   2. first\n\n   3. second\n\n4. last");
    changeListIndent(editor, -1);
    expect(editor.state.doc.toString()).toBe(doc);
    expect(changeListIndent(view("\n\n", 1), 1)).toBe(false);
  });

  it("keeps invalid nesting operations as no-ops and leaves other text to generic commands", () => {
    const first = view("- first\n- second", 2);
    expect(changeListIndent(first, 1)).toBe(true);
    expect(changeListIndent(first, -1)).toBe(true);
    expect(first.state.doc.toString()).toBe("- first\n- second");
    first.dispatch({ selection: { anchor: 0, head: first.state.doc.length } });
    changeListIndent(first, 1);
    expect(first.state.doc.toString()).toBe("- first\n- second");
    expect(changeListIndent(view("plain", 2), 1)).toBe(false);
    expect(changeListIndent(view("```\n- code\n```", 8), 1)).toBe(false);
  });

  it("updates nesting immediately through repeated commands and supports undo/redo", () => {
    const doc = "- parent\n- item";
    const editor = new EditorView({
      parent: document.body,
      state: EditorState.create({ doc, selection: { anchor: doc.length }, extensions: [liveMarkdown({ onLink() {} }), history()] }),
    });
    views.push(editor);
    for (let round = 0; round < 4; round += 1) {
      changeListIndent(editor, 1);
      expect(editor.contentDOM.querySelectorAll('.cm-live-list[style*="--cm-list-depth: 1"]')).toHaveLength(1);
      changeListIndent(editor, -1);
      expect(editor.state.doc.toString()).toBe(doc);
      expect(editor.contentDOM.querySelectorAll('.cm-live-list[style*="--cm-list-depth: 1"]')).toHaveLength(0);
    }
    changeListIndent(editor, 1);
    undo(editor);
    expect(editor.state.doc.toString()).toBe(doc);
    redo(editor);
    expect(editor.state.doc.toString()).toBe("- parent\n  - item");
  });

  it("does not indent a read-only list", () => {
    const editor = new EditorView({
      parent: document.body,
      state: EditorState.create({ doc: "- parent\n- item", selection: { anchor: 15 }, extensions: [liveMarkdown({ onLink() {} }), EditorState.readOnly.of(true)] }),
    });
    views.push(editor);
    expect(changeListIndent(editor, 1)).toBe(false);
    expect(editor.state.doc.toString()).toBe("- parent\n- item");
  });

  it("continues a bullet, a number and a task", () => {
    const bullet = view("- one", 5);
    expect(continueMarkup(bullet)).toBe(true);
    expect(bullet.state.doc.toString()).toBe("- one\n- ");
    const numbered = view("  9) nine", 9);
    continueMarkup(numbered);
    expect(numbered.state.doc.toString()).toBe("  9) nine\n  10) ");
    const task = view("- [x] done", 10);
    continueMarkup(task);
    expect(task.state.doc.toString()).toBe("- [x] done\n- [ ] ");
  });

  it("ends the list on an empty item", () => {
    const empty = view("- one\n- ", 8);
    expect(continueMarkup(empty)).toBe(true);
    expect(empty.state.doc.toString()).toBe("- one\n");
  });

  it("leaves Enter alone inside code and outside text", () => {
    expect(continueMarkup(view("```\n- not a list\n```", 16))).toBe(false);
    expect(continueMarkup(view("# Title", 7))).toBe(false);
  });

  it("starts a new paragraph, quote paragraph or item, so the next line does not join the one above", () => {
    const plain = view("Hello", 5);
    continueMarkup(plain);
    expect(plain.state.doc.toString()).toBe("Hello\n\n");
    const quote = view("> quoted", 8);
    continueMarkup(quote);
    expect(quote.state.doc.toString()).toBe("> quoted\n>\n> ");
    const wrapped = view("- item that\n  wraps", 19);
    continueMarkup(wrapped);
    expect(wrapped.state.doc.toString()).toBe("- item that\n  wraps\n- ");
  });

  it("wraps and unwraps bold", () => {
    const editor = view("make bold", { anchor: 5, head: 9 });
    toggleInline("**")(editor);
    expect(editor.state.doc.toString()).toBe("make **bold**");
    toggleInline("**")(editor);
    expect(editor.state.doc.toString()).toBe("make bold");
  });

  it("makes bold text bold italic rather than italic", () => {
    const inside = view("**bold**", { anchor: 2, head: 6 });
    toggleInline("*")(inside);
    expect(inside.state.doc.toString()).toBe("***bold***");
    toggleInline("*")(inside);
    expect(inside.state.doc.toString()).toBe("**bold**");
    const whole = view("**bold**", { anchor: 0, head: 8 });
    toggleInline("*")(whole);
    expect(whole.state.doc.toString()).toBe("***bold***");
    const italic = view("*it*", { anchor: 1, head: 3 });
    toggleInline("*")(italic);
    expect(italic.state.doc.toString()).toBe("it");
  });

  it("toggles a task from its checkbox without touching anything else", () => {
    const doc = "- [ ] open\n\nelsewhere";
    const editor = view(doc, doc.length);
    const box = editor.contentDOM.querySelector<HTMLInputElement>("input.cm-live-task");
    expect(box).not.toBeNull();
    box!.click();
    expect(editor.state.doc.toString()).toBe("- [x] open\n\nelsewhere");
  });

  it("keeps rendered tables current through edits, a stretch at a time", () => {
    const table = "| a | b |\n| - | - |\n| 1 | 2 |";
    const doc = `${table}\n\nmiddle\n\n${table}\n\nend`;
    const editor = view(doc, doc.length);
    editor.dispatch({ effects: setFocused.of(true) });
    const drawn = () => [...editor.contentDOM.querySelectorAll(".cm-live-table")].map((node) => node.textContent?.replace(/\s+/g, " ").trim());
    expect(drawn()).toEqual(["a b 1 2", "a b 1 2"]);

    // A row typed under the first table, from the line below it.
    const row = doc.indexOf("\n\nmiddle");
    editor.dispatch({ changes: { from: row, insert: "\n| 3 | 4 |" }, selection: { anchor: doc.length + 10 } });
    expect(drawn()).toEqual(["a b 1 2 3 4", "a b 1 2"]);

    // The caret on a table's line leaves it drawn.
    editor.dispatch({ selection: { anchor: 2 } });
    expect(drawn()).toEqual(["a b 1 2 3 4", "a b 1 2"]);

    // Breaking the delimiter row makes it text, not a stale table.
    const delimiter = editor.state.doc.toString().indexOf("| - |");
    editor.dispatch({ changes: { from: delimiter, to: delimiter + 1, insert: "x" } });
    expect(drawn()).toEqual(["a b 1 2"]);
  });

  it("stops drawing a table that a fence opened far above it swallows", () => {
    const doc = `intro\n\n${"filler line\n\n".repeat(3)}| a |\n| - |\n| 1 |\n\nend`;
    const editor = view(doc, 0);
    const drawn = () => editor.contentDOM.querySelectorAll(".cm-live-table").length;
    expect(drawn()).toBe(1);
    editor.dispatch({ changes: { from: 0, insert: "```\n" } });
    expect(drawn()).toBe(0);
    // Closing the fence gives the table back.
    editor.dispatch({ changes: { from: 4, insert: "code\n``\n" } });
    editor.dispatch({ changes: { from: 11, insert: "`" } });
    expect(editor.state.doc.toString().startsWith("```\ncode\n```\n")).toBe(true);
    expect(drawn()).toBe(1);
  });

  it("draws a table an HTML comment closed mid-line releases", () => {
    const doc = "<!--\nstart\n\nmore\n\n| a |\n| - |\n| 1 |\n\nend";
    const editor = view(doc, 0);
    const drawn = () => editor.contentDOM.querySelectorAll(".cm-live-table").length;
    expect(drawn()).toBe(0);
    editor.dispatch({ changes: { from: 10, insert: " -->" } });
    expect(drawn()).toBe(1);
  });

  it("opens a line above a heading from the start of its text", () => {
    const editor = view("# Title\nbody", 2);
    expect(breakBeforeHeading(editor)).toBe(true);
    expect(editor.state.doc.toString()).toBe("\n# Title\nbody");
    expect(editor.state.selection.main.head).toBe(3);
    expect(breakBeforeHeading(view("# Title", 4))).toBe(false);
  });

  it("never lets a link inside a rendered table navigate", () => {
    const doc = "| a |\n| - |\n| [x](./other.md) |\n\nend";
    const editor = view(doc, doc.length);
    const anchor = editor.contentDOM.querySelector<HTMLAnchorElement>(".cm-live-table a");
    expect(anchor?.getAttribute("href")).toBe("./other.md");
    for (const type of ["click", "auxclick"]) {
      const click = new MouseEvent(type, { bubbles: true, cancelable: true, button: type === "click" ? 0 : 1 });
      anchor!.dispatchEvent(click);
      expect(click.defaultPrevented, type).toBe(true);
    }
  });

  it("keeps a link or image that spans lines as source instead of throwing", () => {
    for (const doc of ["[a](x.md\n\"title\")\n\nend", "[a](\nx.md)\n\nend", "![a\nb](data:image/png;base64,AAAA)\n\nend"]) {
      const editor = view(doc, doc.length);
      expect(editor.state.doc.toString()).toBe(doc);
      editor.dispatch({ effects: setFocused.of(true), selection: { anchor: 1 } });
      editor.dispatch({ selection: { anchor: doc.length } });
    }
  });

  it("does not continue a thematic break as a list", () => {
    for (const rule of ["* * *", "- - -"]) {
      expect(continueMarkup(view(rule, rule.length))).toBe(false);
    }
  });

  it("re-enables task checkboxes when the document becomes writable", () => {
    const readOnly = new Compartment();
    const editor = new EditorView({
      state: EditorState.create({
        doc: "- [ ] open\n\nx",
        extensions: [liveMarkdown({ onLink: () => undefined }), readOnly.of(EditorState.readOnly.of(true))],
      }),
      parent: document.body,
    });
    views.push(editor);
    expect(editor.contentDOM.querySelector<HTMLInputElement>(".cm-live-task")!.disabled).toBe(true);
    editor.dispatch({ effects: readOnly.reconfigure(EditorState.readOnly.of(false)) });
    expect(editor.contentDOM.querySelector<HTMLInputElement>(".cm-live-task")!.disabled).toBe(false);
  });

  it("opens a link in a rendered table on a modified click, and only then", () => {
    const onLink = vi.fn();
    const doc = "| a |\n| - |\n| [x](./other.md) |\n\nend";
    const editor = new EditorView({
      state: EditorState.create({ doc, selection: EditorSelection.cursor(doc.length), extensions: liveMarkdown({ onLink }) }),
      parent: document.body,
    });
    views.push(editor);
    const anchor = () => editor.contentDOM.querySelector<HTMLAnchorElement>(".cm-live-table a")!;
    anchor().dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0, ctrlKey: true }));
    expect(onLink).toHaveBeenCalledWith("./other.md");
    expect(editor.state.selection.main.head, "a link click moved the caret into the table").toBe(doc.length);
    anchor().dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(onLink).toHaveBeenCalledTimes(1);
  });

  it("draws a hard-wrapped paragraph as one line, and Backspace across the break joins it in the file", () => {
    const doc = "one two\nthree four\n\nnext";
    const editor = view(doc, 8);
    const lines = [...editor.contentDOM.querySelectorAll(".cm-line")].map((line) => line.textContent);
    expect(lines).toEqual(["one two three four", "", "next"]);
    deleteCharBackward(editor);
    expect(editor.state.doc.toString()).toBe("one twothree four\n\nnext");
  });

  it("deletes a block marker whole with Backspace, and keeps the caret off it", () => {
    const editor = view("## Title\n- [ ] task", 3);
    editor.dispatch({ selection: { anchor: 0 } });
    expect(editor.state.selection.main.head, "a caret placed on hidden markup moves to the text").toBe(3);
    deleteCharBackward(editor);
    expect(editor.state.doc.toString()).toBe("Title\n- [ ] task");
    editor.dispatch({ selection: { anchor: editor.state.doc.length - 4 } });
    deleteCharBackward(editor);
    expect(editor.state.doc.toString()).toBe("Title\n- task");
    deleteCharBackward(editor);
    expect(editor.state.doc.toString()).toBe("Title\ntask");
  });

  it("opens a link on a modified click", () => {
    const onLink = vi.fn();
    const editor = new EditorView({
      state: EditorState.create({ doc: "[a](x.md)", extensions: liveMarkdown({ onLink }) }),
      parent: document.body,
    });
    views.push(editor);
    vi.spyOn(editor, "posAtCoords").mockReturnValue(1);
    editor.contentDOM.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0, ctrlKey: true }));
    expect(onLink).toHaveBeenCalledWith("x.md");
  });
});

describe("WebKitGTK accessible widgets", () => {
  function gtk() {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux x86_64");
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 AppleWebKit/605.1.15");
  }
  afterEach(() => vi.restoreAllMocks());

  it("does not render quote or backslash payloads as images or background URLs", () => {
    gtk();
    for (const payload of ['AAAA"),url("https://tracker.invalid/pixel.png', "AAAA\\evil"]) {
      const doc = `![Picture](<data:image/png;base64,${payload}>)\n\nUnicode – 😀`;
      const editor = view(doc, doc.length);
      expect(editor.contentDOM.querySelector(".cm-live-image")).toBeNull();
      expect(editor.contentDOM.querySelector('[style*="background-image"]')).toBeNull();
    }
  });

  it("rejects the balanced-parenthesis CSS image payload", () => {
    const doc = '![x](data:image/png;base64,AAAA("),url("https://example.invalid/x")/*)\n\nend';
    expect(ranges(doc, doc.length).some((range) => range.kind === "image")).toBe(false);
  });

  it("keeps task actions, checked state and keyboard activation without a native input", () => {
    gtk();
    const doc = "- [ ] open\n\nUnicode – 😀";
    const editor = view(doc, doc.length);
    const checkbox = () => editor.contentDOM.querySelector<HTMLElement>('[role="checkbox"]')!;
    expect(editor.contentDOM.querySelector("input")).toBeNull();
    expect(checkbox().getAttribute("aria-label")).toBe("Open task");
    expect(checkbox().getAttribute("aria-checked")).toBe("false");
    checkbox().dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true }));
    expect(editor.state.doc.toString()).toBe("- [x] open\n\nUnicode – 😀");
    expect(checkbox().getAttribute("aria-checked")).toBe("true");
    checkbox().click();
    expect(editor.state.doc.toString()).toBe(doc);
  });

  it("keeps a read-only task disabled and out of the tab order", () => {
    gtk();
    const doc = "- [ ] open";
    const editor = new EditorView({ parent: document.body,
      state: EditorState.create({ doc, extensions: [liveMarkdown({ onLink() {} }), EditorState.readOnly.of(true)] }),
    });
    views.push(editor);
    const box = editor.contentDOM.querySelector<HTMLElement>('[role="checkbox"]')!;
    expect(box.getAttribute("aria-disabled")).toBe("true");
    expect(box.tabIndex).toBe(-1);
    box.click();
    expect(editor.state.doc.toString()).toBe(doc);
  });

  it("keeps an embedded image's name, intrinsic size and aspect ratio without an img in the editor", () => {
    gtk();
    const create = document.createElement.bind(document);
    const images: HTMLImageElement[] = [];
    vi.spyOn(document, "createElement").mockImplementation(((tag: string, options?: ElementCreationOptions) => {
      const element = create(tag, options);
      if (tag === "img") images.push(element as HTMLImageElement);
      return element;
    }) as typeof document.createElement);
    const doc = "![Picture](data:image/png;base64,AAAA)\n\nUnicode – 😀";
    const editor = view(doc, doc.length);
    const image = editor.contentDOM.querySelector<HTMLElement>('[role="img"]')!;
    expect(editor.contentDOM.querySelector("img.cm-live-image")).toBeNull();
    expect(image.getAttribute("aria-label")).toBe("Picture");
    expect(image.style.backgroundImage).toContain("data:image/png;base64,AAAA");
    const loader = images.find((image) => image.className === "cm-live-image")!;
    Object.defineProperties(loader, { naturalWidth: { value: 240 }, naturalHeight: { value: 120 } });
    loader.dispatchEvent(new Event("load"));
    expect(image.style.width).toBe("240px");
    expect(image.style.aspectRatio).toBe("240 / 120");
  });
});
