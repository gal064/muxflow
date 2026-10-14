// @vitest-environment jsdom
// jsdom, because the commands run against a real EditorView.
import { Compartment, EditorSelection, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it, vi } from "vitest";
import { deleteCharBackward } from "@codemirror/commands";
import { blockMarkupEnd, breakBeforeHeading, continueMarkup, linkAt, linkTarget, liveMarkdown, liveRanges, setFocused, tableRanges, toggleInline, type LiveRange } from "./liveMarkdown";

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

  it("never shows block markers, and keeps each one whole for the caret", () => {
    const doc = "## Title\n\n> - [ ] task\n\n---\n";
    for (const cursor of [5, 18, doc.length - 2]) expect(hidden(doc, cursor)).toEqual(["## ", "> ", "- "]);
    const atomic = ranges(doc, 5).filter((range) => range.kind === "atomic").map((range) => doc.slice(range.from, range.to));
    expect(atomic).toEqual(["## ", "> ", "- ", "[ ] ", "---"]);
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
    expect(drawn).toContainEqual({ kind: "bullet", from: 0, to: 1 });
    expect(drawn).toContainEqual({ kind: "task", from: 8, to: 11, checked: false });
    expect(drawn).toContainEqual({ kind: "task", from: 19, to: 22, checked: true });
    expect(hidden(doc)).toEqual(["- ", "- "]);
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

  it("leaves Enter alone inside code and outside lists", () => {
    expect(continueMarkup(view("```\n- not a list\n```", 16))).toBe(false);
    expect(continueMarkup(view("plain", 5))).toBe(false);
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
