import { redo, undo } from "@codemirror/commands";
import { Facet } from "@codemirror/state";
import { WidgetType, type EditorView } from "@codemirror/view";
import { renderSafeMarkdown } from "./markdown";

/**
 * A table edited where it is drawn, as in Typora: the table never turns back
 * into pipes, and each cell takes typing directly.
 *
 * The source stays the document. A cell edit replaces that cell's text and
 * nothing else, so padding, alignment rows and pipe style the file was written
 * with survive. A cell being edited shows its own source — the cell is the
 * element under the caret — and every other cell stays rendered.
 */

/** A cell's text as source offsets, trimmed. An empty cell is a point inside its pipes. */
export interface CellSpan { from: number; to: number }
export interface RowSpan { from: number; to: number; cells: CellSpan[]; closed: boolean; end: number }
export interface TableLayout {
  /** The header, then each body row. The delimiter row is structure, not content. */
  rows: RowSpan[];
  /** The end of the delimiter row, where a row inserted under the header goes. */
  delimiterEnd: number;
}

/** The rows and cells of a GFM table's source, offset by `base`. */
export function tableLayout(source: string, base = 0): TableLayout {
  const rows: RowSpan[] = [];
  let delimiterEnd = base;
  let at = 0;
  source.split("\n").forEach((line, index) => {
    if (index === 1) delimiterEnd = base + at + line.length;
    else rows.push(rowSpan(line, base + at));
    at += line.length + 1;
  });
  return { rows, delimiterEnd };
}

function rowSpan(line: string, offset: number): RowSpan {
  const start = line.length - line.trimStart().length;
  const end = line.trimEnd().length;
  // Unescaped pipes. GFM splits on a pipe inside a code span too unless it is
  // escaped, so a code span gets no special treatment.
  const edges: number[] = [];
  for (let i = start; i < end; i++) {
    if (line[i] === "\\") i++;
    else if (line[i] === "|") edges.push(i);
  }
  const closed = edges.length > 0 && edges[edges.length - 1] === end - 1;
  if (edges[0] !== start) edges.unshift(start - 1);
  if (edges.length < 2 || !closed) edges.push(end);
  const cells: CellSpan[] = [];
  for (let k = 0; k + 1 < edges.length; k++) {
    const segment = line.slice(edges[k] + 1, edges[k + 1]);
    const trimmed = segment.trim();
    const from = edges[k] + 1 + (trimmed ? segment.length - segment.trimStart().length : Math.min(1, segment.length));
    cells.push({ from: offset + from, to: offset + from + trimmed.length });
  }
  return { from: offset, to: offset + line.length, cells, closed, end: offset + end };
}

/**
 * Cell text as it is written into the source: on one line, with any pipe the
 * user typed escaped so it stays in the cell. A pipe that is already escaped
 * is left alone.
 */
export function cellSource(text: string): string {
  return text
    .replace(/ /g, " ")
    .replace(/\s*[\r\n]+\s*/g, " ")
    .replace(/(\\*)\|/g, (match, slashes: string) => (slashes.length % 2 ? match : `${slashes}\\|`));
}

/**
 * The text a cell writes to the file for `shown`, its `cellSource` form.
 *
 * Trimmed: the space typed before a word is written with the word, not left
 * behind as padding on every keystroke. A trailing backslash would escape the
 * cell's closing pipe and merge it with the next cell, so it is kept apart
 * from the pipe by a space, which the renderer trims.
 */
export function cellWrite(shown: string): string {
  const text = shown.trim();
  return /(^|[^\\])(\\\\)*\\$/.test(text) ? `${text} ` : text;
}

/** The change that sets cell `col` of `row` to `text`, padding a short row with empty cells. */
export function cellChange(layout: TableLayout, row: number, col: number, text: string): { from: number; to: number; insert: string } | undefined {
  const target = layout.rows[row];
  if (!target) return undefined;
  const cell = target.cells[col];
  if (cell) return { from: cell.from, to: cell.to, insert: text };
  const padding = "  |".repeat(col - target.cells.length);
  return { from: target.end, to: target.end, insert: `${target.closed ? "" : " |"}${padding} ${text} |` };
}

/** The change that adds an empty row after `row`; after the header means after the delimiter row. */
export function rowInsertion(layout: TableLayout, row: number): { from: number; insert: string } {
  const columns = layout.rows[0]?.cells.length ?? 1;
  const at = row === 0 ? layout.delimiterEnd : layout.rows[row].to;
  return { from: at, insert: `\n|${"  |".repeat(columns)}` };
}

/** Where an activated link goes; read by widgets, which are built without the options. */
export const linkHandler = Facet.define<(href: string) => void>();

interface TableState {
  source: string;
  readOnly: boolean;
  /** Set while the focused cell's own typing is being written, which the cell already shows. */
  writing: boolean;
}

const tables = new WeakMap<HTMLElement, TableState>();

type Cell = HTMLTableCellElement;

function renderTable(source: string): HTMLElement {
  const holder = document.createElement("div");
  holder.innerHTML = renderSafeMarkdown(source);
  return holder;
}

function cellOf(target: EventTarget | null, wrap: HTMLElement): Cell | undefined {
  const cell = (target as Element | null)?.closest?.("td, th");
  return cell && wrap.contains(cell) ? (cell as Cell) : undefined;
}

/** Whether a cell takes typing: the attribute `draw` sets, which jsdom reads as a browser would. */
function editable(cell: Cell): boolean {
  return cell.hasAttribute("contenteditable");
}

function position(cell: Cell): { row: number; col: number } {
  return { row: (cell.parentElement as HTMLTableRowElement).rowIndex, col: cell.cellIndex };
}

function cellAt(wrap: HTMLElement, row: number, col: number): Cell | undefined {
  return wrap.querySelector("table")?.rows[row]?.cells[col] ?? undefined;
}

function focusedCell(wrap: HTMLElement): Cell | undefined {
  return cellOf(wrap.ownerDocument.activeElement, wrap);
}

function rawText(state: TableState, row: number, col: number): string {
  const cell = tableLayout(state.source).rows[row]?.cells[col];
  return cell ? state.source.slice(cell.from, cell.to) : "";
}

function placeCaret(cell: Cell, at: "start" | "end" | number) {
  const selection = cell.ownerDocument.getSelection();
  if (!selection) return;
  const text = cell.firstChild;
  const length = text?.textContent?.length ?? 0;
  const offset = at === "start" ? 0 : at === "end" ? length : Math.min(at, length);
  const range = cell.ownerDocument.createRange();
  if (text) range.setStart(text, offset);
  else range.setStart(cell, 0);
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
}

/** The caret's offset in the cell's text, or undefined for a selection that is not a caret. */
function caretOffset(cell: Cell): number | undefined {
  const selection = cell.ownerDocument.getSelection();
  if (!selection?.rangeCount || !selection.isCollapsed || !cell.contains(selection.anchorNode)) return undefined;
  const range = cell.ownerDocument.createRange();
  range.selectNodeContents(cell);
  range.setEnd(selection.anchorNode!, selection.anchorOffset);
  return range.toString().length;
}

/** Shows a cell's source for editing, if it differs from what is drawn. */
function showRaw(state: TableState, cell: Cell, at: "start" | "end" | number) {
  const { row, col } = position(cell);
  const raw = rawText(state, row, col);
  if (cell.textContent === raw && !cell.querySelector("*")) return;
  cell.textContent = raw;
  placeCaret(cell, at);
}

/**
 * Brings the drawn table up to date with `state.source`.
 *
 * Patched cell by cell while the shape holds, so a cell being typed into keeps
 * its node, focus and caret; redrawn whole when a row or column came or went.
 */
function draw(wrap: HTMLElement, state: TableState) {
  const fresh = renderTable(state.source);
  const freshTable = fresh.querySelector("table");
  const current = wrap.querySelector("table");
  const focused = focusedCell(wrap);
  const at = focused && position(focused);
  // A table whose drawn rows do not line up with its source rows cannot map an
  // edit to the right cell, so it is shown but not editable.
  const editable = !state.readOnly && !!freshTable && freshTable.rows.length === tableLayout(state.source).rows.length;
  const sameShape = current && freshTable && current.rows.length === freshTable.rows.length
    && [...current.rows].every((row, index) => row.cells.length === freshTable.rows[index].cells.length);
  if (sameShape) {
    [...freshTable.rows].forEach((row, r) => [...row.cells].forEach((cell, c) => {
      const target = current.rows[r].cells[c];
      if (target === focused) {
        if (!state.writing) showRaw(state, target, "end");
        return;
      }
      if (target.innerHTML !== cell.innerHTML) target.innerHTML = cell.innerHTML;
      for (const name of ["align", "style"]) {
        const value = cell.getAttribute(name);
        if (value === null) target.removeAttribute(name);
        else target.setAttribute(name, value);
      }
    }));
  } else {
    wrap.replaceChildren(...fresh.childNodes);
  }
  for (const cell of wrap.querySelectorAll<Cell>("td, th")) {
    if (editable) cell.setAttribute("contenteditable", "plaintext-only");
    else cell.removeAttribute("contenteditable");
  }
  // A keystroke that reshaped the table is followed by `write`, which knows where the caret went.
  if (!sameShape && at && editable && !state.writing) {
    const table = wrap.querySelector("table")!;
    const row = Math.min(at.row, table.rows.length - 1);
    const cell = cellAt(wrap, row, Math.min(at.col, table.rows[row].cells.length - 1));
    if (cell) {
      cell.focus();
      showRaw(state, cell, "end");
    }
  }
}

/** The table's place in the document now, or undefined if the widget and the document disagree. */
function locate(view: EditorView, wrap: HTMLElement, state: TableState): { from: number; to: number; layout: TableLayout } | undefined {
  const from = view.posAtDOM(wrap);
  const to = from + state.source.length;
  if (view.state.doc.sliceString(from, to) !== state.source) return undefined;
  return { from, to, layout: tableLayout(state.source, from) };
}

/** Finds the drawn table that starts at `from`, which a rebuilt widget may have replaced. */
function tableDOM(view: EditorView, from: number): HTMLElement | undefined {
  for (const wrap of view.contentDOM.querySelectorAll<HTMLElement>(".cm-live-table")) {
    if (tables.has(wrap) && view.posAtDOM(wrap) === from) return wrap;
  }
  return undefined;
}

function focusCell(view: EditorView, from: number, row: number, col: number, at: "start" | "end") {
  const wrap = tableDOM(view, from);
  const state = wrap && tables.get(wrap);
  const cell = wrap && cellAt(wrap, row, col);
  if (!state || !cell || !editable(cell)) return false;
  cell.focus();
  showRaw(state, cell, at);
  placeCaret(cell, at);
  return true;
}

/**
 * Moves the caret into the table starting at `from`, from the line above it
 * (`forward`) or below it. The table must be drawn and writable.
 */
export function enterTable(view: EditorView, from: number, forward: boolean): boolean {
  const wrap = tableDOM(view, from);
  const table = wrap?.querySelector("table");
  if (!table) return false;
  const row = forward ? 0 : table.rows.length - 1;
  return focusCell(view, from, row, forward ? 0 : table.rows[row].cells.length - 1, forward ? "start" : "end");
}

/**
 * Leaves the table for the text above or below it.
 *
 * Below a table, a line of text directly under it would be read as another
 * row, so the caret lands after a blank line, which is added if the document
 * has none there. Above, a line of text directly over a table is a paragraph
 * and the table still follows it.
 */
function exitTable(view: EditorView, from: number, to: number, forward: boolean) {
  const doc = view.state.doc;
  view.focus();
  if (!forward) {
    if (from > 0) view.dispatch({ selection: { anchor: from - 1 }, scrollIntoView: true });
    else view.dispatch({ changes: { from: 0, insert: "\n" }, selection: { anchor: 0 }, scrollIntoView: true, userEvent: "input" });
    return;
  }
  const next = doc.sliceString(to, to + 2);
  if (next === "\n\n") view.dispatch({ selection: { anchor: to + 2 }, scrollIntoView: true });
  else {
    const insert = next === "\n" ? { from: to + 1, insert: "\n" } : next === "" ? { from: to, insert: "\n\n" } : { from: to + 1, insert: "\n\n" };
    view.dispatch({ changes: insert, selection: { anchor: to + 2 }, scrollIntoView: true, userEvent: "input" });
  }
}

function write(view: EditorView, wrap: HTMLElement, state: TableState, cell: Cell) {
  const where = locate(view, wrap, state);
  if (!where) return;
  const { row, col } = position(cell);
  const typed = cell.textContent ?? "";
  const shown = cellSource(typed);
  if (shown !== typed) {
    // A newline pasted or a pipe typed: the cell shows what the source holds.
    const caret = caretOffset(cell);
    cell.textContent = shown;
    placeCaret(cell, caret === undefined ? "end" : caret + (shown.length - typed.length));
  }
  const change = cellChange(where.layout, row, col, cellWrite(shown));
  if (!change) return;
  const existing = !!where.layout.rows[row]?.cells[col];
  if (existing) {
    // The space that kept a trailing backslash off the pipe was written with it, and goes with it.
    if (/(^|[^\\])(\\\\)*\\$/.test(view.state.doc.sliceString(change.from, change.to)) && view.state.doc.sliceString(change.to, change.to + 1) === " ") change.to += 1;
    if (view.state.doc.sliceString(change.from, change.to) === change.insert) return;
  }
  const caret = caretOffset(cell);
  state.writing = true;
  try {
    view.dispatch({ changes: change, userEvent: "input.type" });
  } finally {
    state.writing = false;
  }
  // `- ` or `# ` at the start of a row of a table without outer pipes makes
  // the row a list item or a heading, and the table ends above it: the caret
  // follows the text out of the table.
  if (!cell.isConnected || row >= (wrap.querySelector("table")?.rows.length ?? 0)) {
    const lead = shown.length - shown.trimStart().length;
    const offset = Math.max(0, Math.min((caret ?? shown.length) - lead, change.insert.length));
    view.focus();
    view.dispatch({ selection: { anchor: change.from + offset }, scrollIntoView: true });
  }
}

/** Wraps the cell's selected text in `marker`, or unwraps it if it already is, as typing it would. */
export function toggledMarker(text: string, marker: string): string {
  const size = marker.length;
  // `**bold**` starts with `*` but is not italic; `***both***` is.
  const wrapped = text.length >= size * 2 && text.startsWith(marker) && text.endsWith(marker)
    && (size > 1 || !/^\*\*(?!\*)/.test(text));
  return wrapped ? text.slice(size, -size) : `${marker}${text}${marker}`;
}

function wrapSelection(cell: Cell, marker: string) {
  const selection = cell.ownerDocument.getSelection();
  if (!selection?.rangeCount || !cell.contains(selection.anchorNode)) return;
  const before = cell.ownerDocument.createRange();
  before.selectNodeContents(cell);
  before.setEnd(selection.getRangeAt(0).startContainer, selection.getRangeAt(0).startOffset);
  const start = before.toString().length;
  const text = selection.toString();
  const result = toggledMarker(text, marker);
  cell.ownerDocument.execCommand("insertText", false, result);
  // The text stays selected, so the same key undoes it; with nothing
  // selected the caret goes between the markers.
  const unwrapped = result.length < text.length;
  const from = start + (unwrapped ? 0 : marker.length);
  const node = cell.firstChild;
  if (!node) return;
  const range = cell.ownerDocument.createRange();
  range.setStart(node, from);
  range.setEnd(node, from + (unwrapped ? result.length : text.length));
  selection.removeAllRanges();
  selection.addRange(range);
}

function onKey(view: EditorView, wrap: HTMLElement, state: TableState, event: KeyboardEvent) {
  const cell = cellOf(event.target, wrap);
  // WebKit ends a composition before the Enter that confirms it, reporting keyCode 229 for it.
  if (!cell || !editable(cell) || event.isComposing || event.keyCode === 229) return;
  const where = locate(view, wrap, state);
  if (!where) return;
  const { row, col } = position(cell);
  const rows = cell.closest("table")!.rows;
  const lastRow = rows.length - 1;
  const lastCol = rows[row].cells.length - 1;
  const mod = event.metaKey || event.ctrlKey;
  const go = (r: number, c: number, at: "start" | "end") => focusCell(view, where.from, r, c, at);
  const addRow = (after: number, c: number) => {
    view.dispatch({ changes: rowInsertion(where.layout, after), userEvent: "input" });
    go(after + 1, c, "start");
  };
  const handled = () => { event.preventDefault(); event.stopPropagation(); };

  if (mod && !event.altKey) {
    const key = event.key.toLowerCase();
    if (key === "z" || key === "y") {
      handled();
      (key === "y" || event.shiftKey ? redo : undo)(view);
    } else if (key === "b" || key === "i") {
      handled();
      wrapSelection(cell, key === "b" ? "**" : "*");
    }
    return;
  }
  const caret = caretOffset(cell);
  const length = cell.textContent?.length ?? 0;
  // By code too: WebKitGTK can report Shift+Tab as an "Unidentified" key.
  switch (event.code === "Tab" ? "Tab" : event.key) {
    case "Tab":
      handled();
      if (event.shiftKey) {
        if (col > 0) go(row, col - 1, "end");
        else if (row > 0) go(row - 1, rows[row - 1].cells.length - 1, "end");
        else exitTable(view, where.from, where.to, false);
      } else if (col < lastCol) go(row, col + 1, "start");
      else if (row < lastRow) go(row + 1, 0, "start");
      else addRow(row, 0);
      return;
    case "Enter":
      handled();
      if (event.shiftKey) return;
      if (row < lastRow) go(row + 1, col, "start");
      else addRow(row, col);
      return;
    case "ArrowUp":
      handled();
      if (row > 0) go(row - 1, col, "end");
      else exitTable(view, where.from, where.to, false);
      return;
    case "ArrowDown":
      handled();
      if (row < lastRow) go(row + 1, col, "end");
      else exitTable(view, where.from, where.to, true);
      return;
    case "ArrowLeft":
      if (caret !== 0 || event.shiftKey) return;
      handled();
      if (col > 0) go(row, col - 1, "end");
      else if (row > 0) go(row - 1, rows[row - 1].cells.length - 1, "end");
      else exitTable(view, where.from, where.to, false);
      return;
    case "ArrowRight":
      if (caret !== length || event.shiftKey) return;
      handled();
      if (col < lastCol) go(row, col + 1, "start");
      else if (row < lastRow) go(row + 1, 0, "start");
      else exitTable(view, where.from, where.to, true);
      return;
    case "Escape":
      handled();
      exitTable(view, where.from, where.to, true);
      return;
    case "Backspace": {
      // Backspace in an empty body row removes the row.
      const source = where.layout.rows[row];
      if (row === 0 || length > 0 || !source || source.cells.some((span) => span.to > span.from)) return;
      handled();
      view.dispatch({ changes: { from: source.from - 1, to: source.to }, userEvent: "delete" });
      go(row - 1, col, "end");
      return;
    }
  }
}

/** A whole table, drawn by the same sanitizer as the preview, with editable cells. */
export class TableWidget extends WidgetType {
  constructor(readonly source: string, readonly readOnly: boolean) { super(); }
  eq(other: TableWidget) { return other.source === this.source && other.readOnly === this.readOnly; }
  /**
   * A row is drawn about 47px tall (14px text at 1.55, 12px padding top and
   * bottom, a border), not one line of text. Left to estimate from the line
   * count, CodeMirror takes a tall table for a short one after each edit,
   * leaves it out of the stretch it draws, and so takes the focused cell away.
   */
  get estimatedHeight() { return (this.source.split("\n").length - 1) * 47 + 4; }
  toDOM(view: EditorView) {
    const wrap = document.createElement("div");
    wrap.className = "cm-live-table";
    const state: TableState = { source: this.source, readOnly: this.readOnly, writing: false };
    tables.set(wrap, state);
    draw(wrap, state);
    wrap.addEventListener("mousedown", (event) => {
      // A middle button would take focus away, and on Linux paste the primary selection.
      if (event.button === 1) event.preventDefault();
      if (event.button !== 0) return;
      // A modified click on a link opens it, as everywhere else in the
      // document; its href is already sanitized.
      const href = (event.target as Element | null)?.closest?.("a")?.getAttribute("href");
      if (href && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        for (const open of view.state.facet(linkHandler)) open(href);
      }
    });
    // A link in a rendered cell must not activate: a relative href navigates
    // the whole webview off the app, the same hazard the preview guards.
    wrap.addEventListener("click", (event) => event.preventDefault());
    wrap.addEventListener("auxclick", (event) => event.preventDefault());
    wrap.addEventListener("focusin", (event) => {
      const cell = cellOf(event.target, wrap);
      if (!cell || !editable(cell)) return;
      showRaw(state, cell, "end");
      // The editor's own caret waits just above the table, outside it: undoing
      // a cell edit then brings the view back here, not to wherever the caret
      // was before, and a keystroke that reaches the editor cannot land in a row.
      // Later, not now: a cell focused while a reshaped table is redrawn is
      // focused inside the editor's own update, which may not dispatch.
      queueMicrotask(() => {
        const where = locate(view, wrap, state);
        if (where && where.from > 0) view.dispatch({ selection: { anchor: where.from - 1 } });
      });
    });
    wrap.addEventListener("focusout", (event) => {
      const cell = cellOf(event.target, wrap);
      // The window losing focus leaves the cell focused, and its source showing.
      if (!cell || cell.ownerDocument.activeElement === cell) return;
      // Drawn again as rendered Markdown once the caret has left it. Its row is
      // rendered under the table's own header and alignment, not the whole table.
      const { row, col } = position(cell);
      const lines = state.source.split("\n");
      const drawn = renderTable([lines[0], lines[1], ...(row > 0 ? [lines[row + 1]] : [])].join("\n")).querySelector("table")?.rows[row > 0 ? 1 : 0]?.cells[col];
      if (drawn) cell.innerHTML = drawn.innerHTML;
    });
    wrap.addEventListener("input", (event) => {
      const cell = cellOf(event.target, wrap);
      if (cell && editable(cell)) write(view, wrap, state, cell);
    });
    wrap.addEventListener("keydown", (event) => onKey(view, wrap, state, event));
    return wrap;
  }
  updateDOM(dom: HTMLElement) {
    const state = tables.get(dom);
    if (!state) return false;
    // Unless the keystroke changed where the table ends: `- ` or `# ` typed
    // into a table without outer pipes turns its row into a list or a heading.
    const typing = state.writing && state.readOnly === this.readOnly
      && this.source.split("\n").length === state.source.split("\n").length;
    state.source = this.source;
    state.readOnly = this.readOnly;
    // The focused cell's own keystroke changed only that cell, which already shows it.
    if (!typing) draw(dom, state);
    return true;
  }
  ignoreEvent() { return true; }
}
