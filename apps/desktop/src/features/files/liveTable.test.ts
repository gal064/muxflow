// @vitest-environment jsdom
// jsdom, because the table widget is driven through a real EditorView.
import { history } from "@codemirror/commands";
import { EditorSelection, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it } from "vitest";
import { liveMarkdown } from "./liveMarkdown";
import { cellChange, cellSource, cellWrite, rowInsertion, tableLayout, toggledMarker } from "./liveTable";

describe("table layout", () => {
  it("finds each cell's text, skipping the delimiter row", () => {
    const source = "| a | bb |\n|---|---|\n| 1 |  |";
    const { rows, delimiterEnd } = tableLayout(source);
    const text = rows.map((row) => row.cells.map((cell) => source.slice(cell.from, cell.to)));
    expect(text).toEqual([["a", "bb"], ["1", ""]]);
    expect(delimiterEnd).toBe(source.indexOf("\n| 1"));
    // An empty cell is a point after its first space, so typed text lands padded.
    expect(rows[1].cells[1]).toEqual({ from: 27, to: 27 });
  });

  it("splits on unescaped pipes only, with or without outer pipes", () => {
    const source = "a | b\\|c\n- | -\nx | `y`";
    const text = tableLayout(source).rows.map((row) => row.cells.map((cell) => source.slice(cell.from, cell.to)));
    expect(text).toEqual([["a", "b\\|c"], ["x", "`y`"]]);
  });

  it("writes cell text on one line with typed pipes escaped", () => {
    expect(cellSource("a|b")).toBe("a\\|b");
    expect(cellSource("a\\|b")).toBe("a\\|b");
    expect(cellSource("one\ntwo")).toBe("one two");
    expect(cellSource("a b")).toBe("a b");
  });

  it("writes a cell trimmed, and keeps a trailing backslash off the closing pipe", () => {
    expect(cellWrite(" a x ")).toBe("a x");
    expect(cellWrite("a\\")).toBe("a\\ ");
    expect(cellWrite("a\\\\")).toBe("a\\\\");
  });

  it("toggles bold and italic on a cell's selected text", () => {
    expect(toggledMarker("2", "**")).toBe("**2**");
    expect(toggledMarker("**2**", "**")).toBe("2");
    expect(toggledMarker("*it*", "*")).toBe("it");
    expect(toggledMarker("**bold**", "*")).toBe("***bold***");
    expect(toggledMarker("***both***", "*")).toBe("**both**");
  });

  it("pads a short row out to the cell being written", () => {
    const closed = tableLayout("| a | b | c |\n|-|-|-|\n| 1 |");
    expect(cellChange(closed, 1, 2, "z")).toEqual({ from: 27, to: 27, insert: "  | z |" });
    const open = tableLayout("a | b\n-|-\n1");
    expect(cellChange(open, 1, 1, "z")).toEqual({ from: 11, to: 11, insert: " | z |" });
  });

  it("adds a row under the delimiter when asked for one after the header", () => {
    const layout = tableLayout("| a | b |\n|-|-|\n| 1 | 2 |");
    expect(rowInsertion(layout, 0)).toEqual({ from: 15, insert: "\n|  |  |" });
    expect(rowInsertion(layout, 1)).toEqual({ from: 25, insert: "\n|  |  |" });
  });
});

describe("editing a drawn table", () => {
  const views: EditorView[] = [];
  afterEach(() => { for (const view of views.splice(0)) view.destroy(); });

  function mount(doc: string) {
    const view = new EditorView({
      state: EditorState.create({ doc, selection: EditorSelection.cursor(doc.length), extensions: [liveMarkdown({ onLink: () => undefined }), history()] }),
      parent: document.body,
    });
    views.push(view);
    const cell = (row: number, col: number) => view.contentDOM.querySelector("table")!.rows[row].cells[col];
    const type = (row: number, col: number, text: string) => {
      cell(row, col).textContent = text;
      cell(row, col).dispatchEvent(new InputEvent("input", { bubbles: true }));
    };
    const key = (row: number, col: number, init: KeyboardEventInit) => cell(row, col).dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
    return { view, cell, type, key };
  }

  it("changes only the cell typed into, and keeps its node", () => {
    const doc = "| Name   | Value |\n|:-------|------:|\n| alpha  |     1 |\n\nafter";
    const { view, cell, type } = mount(doc);
    const node = cell(1, 0);
    expect(node.getAttribute("contenteditable")).toBe("plaintext-only");
    type(1, 0, "alphabet");
    expect(view.state.doc.toString()).toBe(doc.replace("| alpha  |", "| alphabet  |"));
    expect(cell(1, 0)).toBe(node);
    type(1, 1, "a|b");
    expect(view.state.doc.toString()).toContain("|     a\\|b |");
  });

  it("types word by word without leaving padding behind, and never merges cells over a backslash", () => {
    const { view, type } = mount("|a|b|\n|-|-|\n|1|2|");
    for (const text of ["1 ", "1 x", "1 x ", "1 x y"]) type(1, 0, text);
    expect(view.state.doc.toString()).toBe("|a|b|\n|-|-|\n|1 x y|2|");
    type(1, 0, "1\\");
    type(1, 0, "1\\z");
    expect(view.state.doc.toString()).toBe("|a|b|\n|-|-|\n|1\\z|2|");
  });

  it("redraws when typing in a cell ends the table there", () => {
    const { view, type } = mount("a | b\n-- | --\n1 | 2\n3 | 4");
    type(1, 0, "- 1");
    expect(view.state.doc.toString()).toBe("a | b\n-- | --\n- 1 | 2\n3 | 4");
    // The row became a list item, so the table is the header alone.
    expect(view.contentDOM.querySelector("table")!.rows.length).toBe(1);
    // And the caret followed the text out of the table.
    expect(view.state.selection.main.head).toBe("a | b\n-- | --\n- 1".length);
  });

  it("leaves with Escape onto a line below the table, never inside its last row", () => {
    const { view, key } = mount("| a |\n|---|\n| 1 |\n\nnext");
    key(1, 0, { key: "Escape" });
    expect(view.state.doc.lineAt(view.state.selection.main.head).text).toBe("next");
  });

  it("follows an edit made outside the table", () => {
    const { view, cell } = mount("| a |\n| - |\n| 1 |\n\nend");
    const at = view.state.doc.toString().indexOf("1");
    view.dispatch({ changes: { from: at, to: at + 1, insert: "**two**" } });
    expect(cell(1, 0).innerHTML).toBe("<strong>two</strong>");
  });

  it("adds a row from the last cell, and removes an empty one with Backspace", () => {
    const { view, key } = mount("| a | b |\n|---|---|\n| 1 | 2 |");
    key(1, 1, { key: "Tab" });
    expect(view.state.doc.toString()).toBe("| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |");
    key(2, 0, { key: "Backspace" });
    expect(view.state.doc.toString()).toBe("| a | b |\n|---|---|\n| 1 | 2 |");
    key(1, 0, { key: "Enter" });
    expect(view.state.doc.toString()).toBe("| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |");
  });

  it("leaves below the table onto a line of its own, not into a new row", () => {
    const { view, key } = mount("| a |\n|---|\n| 1 |");
    key(1, 0, { key: "ArrowDown" });
    expect(view.state.doc.toString()).toBe("| a |\n|---|\n| 1 |\n\n");
    expect(view.state.selection.main.head).toBe(view.state.doc.length);
  });

  it("undoes a cell edit from inside the cell", () => {
    const { view, cell, type, key } = mount("| a |\n| - |\n| 1 |\n\nend");
    type(1, 0, "12");
    expect(view.state.doc.toString()).toContain("| 12 |");
    key(1, 0, { key: "z", ctrlKey: true });
    expect(view.state.doc.toString()).toContain("| 1 |");
    expect(cell(1, 0).textContent).toBe("1");
  });

  it("is not editable in a read-only document", () => {
    const view = new EditorView({
      state: EditorState.create({ doc: "| a |\n| - |\n| 1 |", extensions: [liveMarkdown({ onLink: () => undefined }), EditorState.readOnly.of(true)] }),
      parent: document.body,
    });
    views.push(view);
    expect(view.contentDOM.querySelector("td")!.hasAttribute("contenteditable")).toBe(false);
  });
});
