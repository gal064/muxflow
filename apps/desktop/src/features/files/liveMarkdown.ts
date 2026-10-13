import { HighlightStyle, Language, LanguageSupport, defineLanguageFacet, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { EditorSelection, EditorState, RangeSet, StateEffect, StateField, type Extension, type Range } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin, WidgetType, type Command, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import type { SyntaxNode, SyntaxNodeRef } from "@lezer/common";
import { tags } from "@lezer/highlight";
import { GFM, parser } from "@lezer/markdown";
import { renderSafeMarkdown } from "./markdown";

/**
 * Markdown edited in place: the text is the document, and the syntax is drawn
 * away wherever the user is not working.
 *
 * Nothing here ever rewrites the source. A rich-text editor would hold its own
 * document and serialize Markdown back out on save, and that serialization
 * normalizes — bullet characters, emphasis markers, table padding, hard wraps —
 * so opening an agent-written file and typing one character would rewrite it
 * from top to bottom. Here every rendering is a decoration over the exact
 * bytes on disk, and the bytes change only where the user typed.
 *
 * `@codemirror/lang-markdown` is not used because it statically pulls the HTML,
 * CSS and JavaScript languages in for embedded blocks; the parser is wrapped
 * directly instead.
 */
const markdownLanguage = new Language(
  defineLanguageFacet({ commentTokens: { block: { open: "<!--", close: "-->" } } }),
  parser.configure(GFM),
  [],
  "markdown",
);

/** Only embedded raster images render, the same policy as the preview. */
const SAFE_EMBEDDED_IMAGE = /^data:image\/(?:png|gif|jpe?g|webp);base64,/i;

/** Whether the editor has focus. Syntax is only revealed for a user who is in it. */
export const setFocused = StateEffect.define<boolean>();
const editorFocused = StateField.define<boolean>({
  create: () => false,
  update: (value, transaction) => {
    for (const effect of transaction.effects) if (effect.is(setFocused)) value = effect.value;
    return value;
  },
});

/**
 * The line spans the selection touches. A construct on one of these lines shows
 * its source; everywhere else it is rendered.
 *
 * Per line rather than per element: an element that reveals its markers as the
 * caret crosses into it shifts the text under the caret mid-word, while a line
 * changes shape once, when the caret arrives.
 */
export function activeLines(state: EditorState): ReadonlyArray<{ from: number; to: number }> {
  if (!state.field(editorFocused, false)) return [];
  return state.selection.ranges.map((range) => ({
    from: state.doc.lineAt(range.from).from,
    to: state.doc.lineAt(range.to).to,
  }));
}

function touches(active: ReadonlyArray<{ from: number; to: number }>, from: number, to: number): boolean {
  return active.some((line) => line.from <= to && line.to >= from);
}

/** One drawn change to the source, described without the view so it can be tested. */
export type LiveRange =
  | { kind: "hide"; from: number; to: number }
  | { kind: "line"; from: number; className: string }
  | { kind: "mark"; from: number; to: number; className: string }
  | { kind: "bullet"; from: number; to: number }
  | { kind: "task"; from: number; to: number; checked: boolean }
  | { kind: "rule"; from: number; to: number }
  | { kind: "image"; from: number; to: number; src: string; alt: string };

const HEADING = /^(?:ATX|Setext)Heading([1-6])$/;

/**
 * The decorations for `[from, to)`, which in the view is one visible range.
 *
 * Line styling is applied whether or not the line is active, so the page keeps
 * its shape as the caret moves; only markers come and go.
 */
export function liveRanges(state: EditorState, from: number, to: number): LiveRange[] {
  const active = activeLines(state);
  const doc = state.doc;
  const out: LiveRange[] = [];
  // Clamped to the range: a long block that merely overlaps it is styled one
  // visible range at a time, not end to end on every keystroke.
  const lineClass = (start: number, end: number, className: string) => {
    for (let pos = Math.max(start, from), last = Math.min(end, to); pos <= last;) {
      const line = doc.lineAt(pos);
      out.push({ kind: "line", from: line.from, className });
      pos = line.to + 1;
    }
  };
  const hideUnless = (node: { from: number; to: number }, extend = 0) => {
    if (!touches(active, node.from, node.to)) out.push({ kind: "hide", from: node.from, to: Math.min(node.to + extend, doc.length) });
  };
  // A marker followed by its separating space hides with it, or the text would
  // keep a ragged indent where the marker was.
  const spaceAfter = (pos: number) => (doc.sliceString(pos, pos + 1) === " " ? 1 : 0);

  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      const name = node.name;
      const heading = HEADING.exec(name);
      if (heading) {
        // A setext heading's underline is a line of its own; styled as the
        // heading it would draw a second rule under an empty row.
        lineClass(node.from, name.startsWith("Setext") ? node.from : node.to, `cm-live-h${heading[1]}`);
        return;
      }
      switch (name) {
        case "HeaderMark": {
          const parent = node.node.parent;
          if (parent?.name.startsWith("Setext")) {
            hideUnless({ from: node.from, to: node.to });
            break;
          }
          // An opening mark takes its space with it; a closing one, the space before it.
          const closing = node.from > doc.lineAt(node.from).from && doc.sliceString(node.from - 1, node.from) === " ";
          if (closing) hideUnless({ from: node.from - 1, to: node.to });
          else hideUnless(node, spaceAfter(node.to));
          break;
        }
        case "EmphasisMark":
        case "StrikethroughMark":
          hideUnless(node);
          break;
        case "Strikethrough":
          out.push({ kind: "mark", from: node.from, to: node.to, className: "cm-live-strike" });
          break;
        case "InlineCode":
          out.push({ kind: "mark", from: node.from, to: node.to, className: "cm-live-inline-code" });
          break;
        case "CodeMark":
          if (node.node.parent?.name === "InlineCode") hideUnless(node);
          break;
        case "Escape":
          hideUnless({ from: node.from, to: node.from + 1 });
          break;
        case "FencedCode":
        case "CodeBlock":
          liveCodeBlock(node, doc, active, out, from, to);
          return false;
        case "Blockquote":
          lineClass(node.from, node.to, "cm-live-quote");
          break;
        case "QuoteMark":
          hideUnless(node, spaceAfter(node.to));
          break;
        case "ListMark":
          liveListMark(node, doc, active, out);
          break;
        case "TaskMarker":
          if (!touches(active, node.from, node.to)) {
            out.push({ kind: "task", from: node.from, to: node.to, checked: /x/i.test(doc.sliceString(node.from, node.to)) });
          }
          break;
        case "HorizontalRule":
          if (!touches(active, node.from, node.to)) out.push({ kind: "rule", from: node.from, to: node.to });
          break;
        case "Link":
        case "Image":
          liveLink(node.node, doc, active, out);
          return false;
        case "Autolink":
          liveAutolink(node.node, active, out);
          return false;
        case "URL":
          // A bare GFM autolink; anything inside a link was handled with it.
          out.push({ kind: "mark", from: node.from, to: node.to, className: "cm-live-link" });
          break;
        case "Table":
          // Rendered whole by the table field, which is the only place a
          // replacement spanning lines may come from.
          return false;
      }
      return undefined;
    },
  });
  // A link's destination or an image's alt text may continue on the next line.
  // CodeMirror throws on a plugin replacement that spans a line break — out of
  // the view's constructor, which takes the whole app down — so those stay
  // source.
  return out.filter((range) => range.kind === "line" || range.kind === "mark" || doc.lineAt(range.from).to >= range.to);
}

type Doc = EditorState["doc"];
type Active = ReadonlyArray<{ from: number; to: number }>;

function liveCodeBlock(node: SyntaxNodeRef, doc: Doc, active: Active, out: LiveRange[], from: number, to: number) {
  const first = doc.lineAt(node.from);
  const last = doc.lineAt(node.to);
  for (let pos = Math.max(first.from, doc.lineAt(from).from), end = Math.min(last.to, to); pos <= end;) {
    const line = doc.lineAt(pos);
    const edge = `${line.number === first.number ? " cm-live-code-first" : ""}${line.number === last.number ? " cm-live-code-last" : ""}`;
    out.push({ kind: "line", from: line.from, className: `cm-live-code${edge}` });
    pos = line.to + 1;
  }
  if (node.name !== "FencedCode" || touches(active, node.from, node.to)) return;
  // The fences, but not the info string: the language name stays as a label.
  const cursor = node.node.cursor();
  if (!cursor.firstChild()) return;
  do {
    if (cursor.name === "CodeMark") out.push({ kind: "hide", from: cursor.from, to: cursor.to });
  } while (cursor.nextSibling());
}

function liveListMark(node: SyntaxNodeRef, doc: Doc, active: Active, out: LiveRange[]) {
  const item = node.node.parent;
  if (item?.parent?.name !== "BulletList") return;
  if (touches(active, node.from, node.to)) return;
  const end = Math.min(node.to + (doc.sliceString(node.to, node.to + 1) === " " ? 1 : 0), doc.length);
  // A task item is drawn as its checkbox alone.
  if (item.getChild("Task")) out.push({ kind: "hide", from: node.from, to: end });
  else out.push({ kind: "bullet", from: node.from, to: node.to });
}

function liveLink(node: SyntaxNode, doc: Doc, active: Active, out: LiveRange[]) {
  const marks = node.getChildren("LinkMark");
  const url = node.getChild("URL");
  const textFrom = marks[0]?.to ?? node.from;
  const textTo = marks[1]?.from ?? node.to;
  if (node.name === "Image" && url && !touches(active, node.from, node.to)) {
    const src = doc.sliceString(url.from, url.to);
    if (SAFE_EMBEDDED_IMAGE.test(src)) {
      out.push({ kind: "image", from: node.from, to: node.to, src, alt: doc.sliceString(textFrom, textTo) });
      return;
    }
  }
  out.push({ kind: "mark", from: textFrom, to: textTo, className: node.name === "Image" ? "cm-live-image-alt" : "cm-live-link" });
  if (touches(active, node.from, node.to) || marks.length < 2) return;
  out.push({ kind: "hide", from: node.from, to: marks[0].to });
  out.push({ kind: "hide", from: marks[1].from, to: node.to });
}

function liveAutolink(node: SyntaxNode, active: Active, out: LiveRange[]) {
  const url = node.getChild("URL");
  if (!url) return;
  out.push({ kind: "mark", from: url.from, to: url.to, className: "cm-live-link" });
  if (touches(active, node.from, node.to)) return;
  out.push({ kind: "hide", from: node.from, to: url.from });
  out.push({ kind: "hide", from: url.to, to: node.to });
}

/** The link target under a document position, if the position is in a link. */
export function linkAt(state: EditorState, pos: number): string | undefined {
  for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1); node; node = node.parent) {
    if (node.name === "URL" && node.parent?.name !== "Image") return linkTarget(state.doc.sliceString(node.from, node.to));
    if (node.name === "Link" || node.name === "Autolink") {
      const url = node.getChild("URL");
      return url ? linkTarget(state.doc.sliceString(url.from, url.to)) : undefined;
    }
    if (node.name === "Image") return undefined;
  }
  return undefined;
}

/**
 * A destination as written, resolved the way the preview's renderer resolves
 * it: `<…>` brackets dropped, backslash escapes undone, a bare `www.` link
 * read as http and a bare address as mail.
 */
export function linkTarget(raw: string): string {
  const target = (raw.startsWith("<") && raw.endsWith(">") ? raw.slice(1, -1) : raw).replace(/\\([!-/:-@[-`{-~])/g, "$1");
  if (/^www\./i.test(target)) return `http://${target}`;
  return /^[^\s/:@]+@[^\s/:@]+\.[^\s/:@]+$/.test(target) ? `mailto:${target}` : target;
}

class BulletWidget extends WidgetType {
  eq() { return true; }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-live-bullet";
    span.textContent = "•";
    return span;
  }
}

class RuleWidget extends WidgetType {
  eq() { return true; }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-live-rule";
    return span;
  }
}

class TaskWidget extends WidgetType {
  constructor(readonly checked: boolean, readonly readOnly: boolean) { super(); }
  eq(other: TaskWidget) { return other.checked === this.checked && other.readOnly === this.readOnly; }
  toDOM(view: EditorView) {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "cm-live-task";
    box.checked = this.checked;
    box.disabled = this.readOnly;
    box.setAttribute("aria-label", this.checked ? "Completed task" : "Open task");
    box.addEventListener("mousedown", (event) => event.preventDefault());
    box.addEventListener("click", (event) => {
      event.preventDefault();
      // The position is read now rather than captured at construction: the
      // widget outlives edits above it.
      const pos = view.posAtDOM(box);
      const marker = view.state.doc.sliceString(pos, pos + 3);
      if (view.state.readOnly || !/^\[[ xX]\]$/.test(marker)) return;
      view.dispatch({ changes: { from: pos + 1, to: pos + 2, insert: marker[1] === " " ? "x" : " " } });
    });
    return box;
  }
  ignoreEvent() { return true; }
}

class ImageWidget extends WidgetType {
  constructor(readonly src: string, readonly alt: string) { super(); }
  eq(other: ImageWidget) { return other.src === this.src && other.alt === this.alt; }
  toDOM() {
    const image = document.createElement("img");
    image.className = "cm-live-image";
    image.src = this.src;
    image.alt = this.alt;
    return image;
  }
}

/** A whole table, drawn by the same sanitizer as the preview. */
class TableWidget extends WidgetType {
  constructor(readonly source: string) { super(); }
  eq(other: TableWidget) { return other.source === this.source; }
  toDOM(view: EditorView) {
    const wrap = document.createElement("div");
    wrap.className = "cm-live-table";
    wrap.innerHTML = renderSafeMarkdown(this.source);
    // Clicking a rendered table is asking to edit it: the caret goes to the
    // start of the source, which reveals it.
    wrap.addEventListener("mousedown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      view.dispatch({ selection: { anchor: view.posAtDOM(wrap) } });
      view.focus();
    });
    // A link in a rendered cell must not activate: a relative href navigates
    // the whole webview off the app, the same hazard the preview guards.
    wrap.addEventListener("click", (event) => event.preventDefault());
    wrap.addEventListener("auxclick", (event) => event.preventDefault());
    return wrap;
  }
  ignoreEvent() { return true; }
}

const bullet = Decoration.replace({ widget: new BulletWidget() });
const rule = Decoration.replace({ widget: new RuleWidget() });
const hidden = Decoration.replace({});

function toDecoration(range: LiveRange, readOnly: boolean): Range<Decoration> {
  switch (range.kind) {
    case "hide": return hidden.range(range.from, range.to);
    case "line": return Decoration.line({ class: range.className }).range(range.from);
    case "mark": return Decoration.mark({ class: range.className }).range(range.from, range.to);
    case "bullet": return bullet.range(range.from, range.to);
    case "rule": return rule.range(range.from, range.to);
    case "task": return Decoration.replace({ widget: new TaskWidget(range.checked, readOnly) }).range(range.from, range.to);
    case "image": return Decoration.replace({ widget: new ImageWidget(range.src, range.alt) }).range(range.from, range.to);
  }
}

function buildDecorations(view: EditorView): DecorationSet {
  const ranges: Range<Decoration>[] = [];
  for (const visible of view.visibleRanges) {
    for (const range of liveRanges(view.state, visible.from, visible.to)) {
      if (range.kind === "hide" && range.from === range.to) continue;
      ranges.push(toDecoration(range, view.state.readOnly));
    }
  }
  return Decoration.set(ranges, true);
}

const livePlugin = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  constructor(view: EditorView) { this.decorations = buildDecorations(view); }
  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged || update.selectionSet
      || update.startState.readOnly !== update.state.readOnly
      || update.startState.field(editorFocused) !== update.state.field(editorFocused)
      || syntaxTree(update.startState) !== syntaxTree(update.state)) {
      this.decorations = buildDecorations(update.view);
    }
  }
}, { decorations: (plugin) => plugin.decorations });

/** The block containers a table can sit inside. Everything else is skipped whole. */
const TABLE_CONTAINERS = new Set(["Document", "Blockquote", "BulletList", "OrderedList", "ListItem"]);

/** The inactive tables overlapping `[from, to]`, each as its whole source. */
export function tableRanges(state: EditorState, from = 0, to = state.doc.length): Array<{ from: number; to: number; source: string }> {
  const active = activeLines(state);
  const out: Array<{ from: number; to: number; source: string }> = [];
  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      if (node.name === "Table") {
        const start = state.doc.lineAt(node.from).from;
        const end = state.doc.lineAt(node.to).to;
        // A table inside a quote or a list would take the markers on its lines
        // with it, so only one that starts its line is drawn whole.
        if (start === node.from && !touches(active, start, end)) out.push({ from: start, to: end, source: state.doc.sliceString(start, end) });
        return false;
      }
      return TABLE_CONTAINERS.has(node.name);
    },
  });
  return out;
}

function tableDecoration(table: { from: number; to: number; source: string }): Range<Decoration> {
  return Decoration.replace({ widget: new TableWidget(table.source), block: true }).range(table.from, table.to);
}

/**
 * Redraws the tables in each stretch, leaving the rest of the set as it was.
 *
 * Each stretch is widened to whole lines and one character either side, so a
 * line typed directly above or below a table still reaches it, and then to the
 * full extent of every table found, so a table is always replaced whole.
 */
function refreshTables(value: DecorationSet, state: EditorState, stretches: ReadonlyArray<readonly [number, number]>): DecorationSet {
  const doc = state.doc;
  for (const [start, end] of stretches) {
    const from = Math.max(0, doc.lineAt(Math.min(start, doc.length)).from - 1);
    const to = Math.min(doc.length, doc.lineAt(Math.min(end, doc.length)).to + 1);
    const found = tableRanges(state, from, to);
    let filterFrom = from;
    let filterTo = to;
    for (const table of found) {
      filterFrom = Math.min(filterFrom, table.from);
      filterTo = Math.max(filterTo, table.to);
    }
    value = value.update({ filterFrom, filterTo, filter: () => false, add: found.map(tableDecoration), sort: true });
  }
  return value;
}

/**
 * Every inactive table, drawn whole.
 *
 * Kept up to date by stretch rather than rebuilt. A full rebuild walks the
 * block structure of the entire document and makes a widget per table, on
 * every keystroke and every caret move; on a 2 MB file that was most of the
 * cost of typing a character. Only three things can change a table's face:
 * an edit near it, the caret arriving or leaving, and the parser reaching it.
 */
const tableField = StateField.define<DecorationSet>({
  create: (state) => RangeSet.of(tableRanges(state).map(tableDecoration)),
  update: (value, transaction) => {
    const before = transaction.startState;
    const after = transaction.state;
    const stretches: Array<readonly [number, number]> = [];
    if (transaction.docChanged) {
      value = value.map(transaction.changes);
      transaction.changes.iterChangedRanges((_fromA, _toA, fromB, toB) => { stretches.push([fromB, toB]); });
    }
    if (transaction.docChanged || transaction.selection || before.field(editorFocused) !== after.field(editorFocused)) {
      for (const line of activeLines(before)) {
        stretches.push([transaction.changes.mapPos(line.from), transaction.changes.mapPos(line.to)]);
      }
      for (const line of activeLines(after)) stretches.push([line.from, line.to]);
    }
    const treeBefore = syntaxTree(before);
    const treeAfter = syntaxTree(after);
    // The parser working through the document in the background: the newly
    // parsed stretch may hold tables nobody has drawn yet.
    if (!transaction.docChanged && treeBefore !== treeAfter) {
      stretches.push([Math.min(treeBefore.length, treeAfter.length), treeAfter.length]);
    }
    return stretches.length ? refreshTables(value, after, stretches) : value;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const liveHighlight = HighlightStyle.define([
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strong, fontWeight: "650" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.heading, fontWeight: "650", color: "var(--chrome-ink-strong)" },
  { tag: tags.link, color: "var(--accent)" },
  { tag: tags.url, color: "var(--chrome-dim)" },
  { tag: tags.monospace, fontFamily: "var(--font-mono)" },
  { tag: tags.quote, color: "var(--chrome-dim)" },
  { tag: [tags.processingInstruction, tags.contentSeparator, tags.labelName], color: "var(--chrome-faint)" },
  { tag: tags.comment, color: "var(--chrome-faint)" },
]);

// ---------------------------------------------------------------------------
// Commands

const LIST_ITEM = /^(\s*(?:>\s?)*\s*)([-*+]|(\d+)([.)]))(\s+)(\[[ xX]\]\s+)?/;
const QUOTE_ONLY = /^(\s*>\s?)+/;

function inCode(state: EditorState, pos: number): boolean {
  for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, -1); node; node = node.parent) {
    if (node.name === "FencedCode" || node.name === "CodeBlock" || node.name === "InlineCode") return true;
  }
  return false;
}

/**
 * Enter at the end of a list item starts the next item; Enter on an empty item
 * ends the list. Blockquotes continue the same way.
 */
export const continueMarkup: Command = (view) => {
  const { state } = view;
  const range = state.selection.main;
  if (state.selection.ranges.length > 1 || !range.empty || state.readOnly) return false;
  const line = state.doc.lineAt(range.head);
  if (inCode(state, range.head)) return false;
  // The parser's word, not the pattern's: `* * *` matches but is a rule.
  const candidate = LIST_ITEM.exec(line.text);
  const item = candidate && syntaxTree(state).resolveInner(line.from + candidate[1].length, 1).name === "ListMark" ? candidate : undefined;
  const quote = item ? undefined : QUOTE_ONLY.exec(line.text);
  const marker = item ?? quote;
  if (!marker || range.head < line.from + marker[0].length) return false;
  const rest = line.text.slice(marker[0].length);
  if (!rest.trim() && range.head === line.to) {
    // An empty item: the user is done with the list.
    const prefix = item ? item[1] : "";
    view.dispatch({ changes: { from: line.from, to: line.to, insert: prefix }, selection: { anchor: line.from + prefix.length }, userEvent: "input" });
    return true;
  }
  let next = marker[0];
  if (item) {
    const number = item[3] ? `${Number(item[3]) + 1}${item[4]}` : item[2];
    next = `${item[1]}${number}${item[5]}${item[6] ? "[ ] " : ""}`;
  }
  view.dispatch(state.update(state.replaceSelection(`${state.lineBreak}${next}`), { scrollIntoView: true, userEvent: "input" }));
  return true;
};

/** How many `char` sit next to `pos`, going in `direction`. */
function run(state: EditorState, pos: number, direction: 1 | -1, char: string): number {
  let count = 0;
  while (state.doc.sliceString(direction < 0 ? pos - count - 1 : pos + count, direction < 0 ? pos - count : pos + count + 1) === char) count += 1;
  return count;
}

/** Wraps the selection in `marker`, or unwraps it if it already is. */
export function toggleInline(marker: string): Command {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const size = marker.length;
    view.dispatch(state.changeByRange((range) => {
      const before = state.doc.sliceString(range.from - size, range.from);
      const after = state.doc.sliceString(range.to, range.to + size);
      // Around `**bold**` the nearest `*` is half of the bold marker; an odd run
      // on both sides (`*it*`, `***both***`) is the one that holds an italic.
      const italic = size > 1 || (run(state, range.from, -1, marker) % 2 === 1 && run(state, range.to, 1, marker) % 2 === 1);
      if (before === marker && after === marker && italic) {
        return {
          changes: [{ from: range.from - size, to: range.from }, { from: range.to, to: range.to + size }],
          range: EditorSelection.range(range.from - size, range.to - size),
        };
      }
      const inner = state.doc.sliceString(range.from, range.to);
      const innerLonger = size === 1 && inner[1] === marker && inner[inner.length - 2] === marker;
      if (inner.length >= size * 2 && inner.startsWith(marker) && inner.endsWith(marker) && !innerLonger) {
        return {
          changes: [{ from: range.from, to: range.from + size }, { from: range.to - size, to: range.to }],
          range: EditorSelection.range(range.from, range.to - size * 2),
        };
      }
      return {
        changes: [{ from: range.from, insert: marker }, { from: range.to, insert: marker }],
        range: EditorSelection.range(range.from + size, range.to + size),
      };
    }), { userEvent: "input" });
    return true;
  };
}

export interface LiveMarkdownOptions {
  /** A link was activated with the platform modifier held. */
  onLink(href: string): void;
}

/** The language, the live rendering, and the editing behaviour, as one extension. */
export function liveMarkdown(options: LiveMarkdownOptions): Extension {
  return [
    new LanguageSupport(markdownLanguage),
    syntaxHighlighting(liveHighlight),
    editorFocused,
    EditorView.focusChangeEffect.of((_state, focusing) => setFocused.of(focusing)),
    livePlugin,
    tableField,
    EditorView.lineWrapping,
    EditorView.domEventHandlers({
      mousedown: (event, view) => {
        if (event.button !== 0 || !(event.metaKey || event.ctrlKey)) return false;
        const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
        const href = pos === null ? undefined : linkAt(view.state, pos);
        if (!href) return false;
        event.preventDefault();
        options.onLink(href);
        return true;
      },
    }),
  ];
}
