import { HighlightStyle, Language, LanguageSupport, defineLanguageFacet, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { EditorSelection, EditorState, Prec, RangeSet, StateEffect, StateField, countColumn, type ChangeSpec, type Extension, type Range, type Text, type Transaction } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin, WidgetType, keymap, type Command, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import type { SyntaxNode, SyntaxNodeRef } from "@lezer/common";
import { tags } from "@lezer/highlight";
import { GFM, parser } from "@lezer/markdown";
import { TableWidget, enterTable, linkHandler } from "./liveTable";

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
 * The selection, while the editor has focus. An inline element the selection
 * touches — bold, a link, inline code — shows its markers; everything else
 * stays rendered, as in Typora. Headings reveal their markers too; other
 * block markers stay rendered and are kept whole for the caret.
 */
function activeRanges(state: EditorState): ReadonlyArray<{ from: number; to: number }> {
  if (!state.field(editorFocused, false)) return [];
  return state.selection.ranges;
}

function touches(active: ReadonlyArray<{ from: number; to: number }>, from: number, to: number): boolean {
  return active.some((line) => line.from <= to && line.to >= from);
}

/** One drawn change to the source, described without the view so it can be tested. */
export type LiveRange =
  | { kind: "hide"; from: number; to: number }
  | { kind: "line"; from: number; className: string; style?: string }
  | { kind: "mark"; from: number; to: number; className: string }
  | { kind: "bullet"; from: number; to: number }
  | { kind: "task"; from: number; to: number; checked: boolean }
  | { kind: "rule"; from: number; to: number }
  | { kind: "image"; from: number; to: number; src: string; alt: string }
  /** A block marker and its space, kept whole: the caret never stops inside it, and Backspace takes it at once. */
  | { kind: "atomic"; from: number; to: number };

const HEADING = /^(?:ATX|Setext)Heading([1-6])$/;

/**
 * The decorations for `[from, to)`, which in the view is one visible range.
 *
 * Line styling is applied whether or not the line is active, so the page keeps
 * its shape as the caret moves; only markers come and go.
 */
export function liveRanges(state: EditorState, from: number, to: number): LiveRange[] {
  const active = activeRanges(state);
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
  /** Hides `node` unless the selection touches `element`, the construct it marks. */
  const hideUnless = (node: { from: number; to: number }, element: { from: number; to: number } = node) => {
    if (!touches(active, element.from, element.to)) out.push({ kind: "hide", from: node.from, to: node.to });
  };
  // A marker followed by its separating space hides with it, or the text would
  // keep a ragged indent where the marker was.
  const spaceAfter = (pos: number) => (doc.sliceString(pos, pos + 1) === " " ? 1 : 0);
  const blockMark = (markFrom: number, markTo: number, hide: boolean) => {
    const end = Math.min(markTo + spaceAfter(markTo), doc.length);
    if (hide) out.push({ kind: "hide", from: markFrom, to: end });
    out.push({ kind: "atomic", from: markFrom, to: end });
  };

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
            // The underline is a line of its own, shown while the heading is being edited.
            hideUnless(node, parent);
            break;
          }
          if (parent && touches(active, parent.from, parent.to)) break;
          // An opening mark takes its space with it; a closing one, the space before it.
          const closing = node.from > doc.lineAt(node.from).from && doc.sliceString(node.from - 1, node.from) === " ";
          if (closing) out.push({ kind: "hide", from: node.from - 1, to: node.to });
          else blockMark(node.from, node.to, true);
          break;
        }
        case "EmphasisMark":
        case "StrikethroughMark":
          hideUnless(node, node.node.parent ?? node);
          break;
        case "Strikethrough":
          out.push({ kind: "mark", from: node.from, to: node.to, className: "cm-live-strike" });
          break;
        case "InlineCode":
          out.push({ kind: "mark", from: node.from, to: node.to, className: "cm-live-inline-code" });
          break;
        case "CodeMark":
          if (node.node.parent?.name === "InlineCode") hideUnless(node, node.node.parent);
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
          blockMark(node.from, node.to, true);
          break;
        case "ListMark":
          liveListLayout(node, state, out);
          liveListMark(node, doc, out, blockMark);
          break;
        case "Paragraph":
        case "Task":
          liveListParagraph(node, state, out, from, to);
          break;
        case "TaskMarker": {
          const end = node.to + /^[ \t]*/.exec(doc.sliceString(node.to, doc.lineAt(node.to).to))![0].length;
          out.push({ kind: "task", from: node.from, to: end, checked: /x/i.test(doc.sliceString(node.from, node.to)) });
          out.push({ kind: "atomic", from: node.from, to: end });
          break;
        }
        case "HorizontalRule":
          out.push({ kind: "rule", from: node.from, to: node.to });
          out.push({ kind: "atomic", from: node.from, to: node.to });
          break;
        case "Link":
        case "Image":
          // `[text]` alone is a reference to a definition elsewhere; the
          // preview draws it as a link only when one exists, and as plain text
          // otherwise. It stays text here. `[text][ref]` is drawn as a link.
          if (!node.node.getChild("URL") && !node.node.getChild("LinkLabel")) {
            for (const mark of node.node.getChildren("LinkMark")) out.push({ kind: "mark", from: mark.from, to: mark.to, className: "cm-live-plain" });
            break;
          }
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
  return out.filter((range) => range.kind === "line" || range.kind === "mark" || range.kind === "atomic" || doc.lineAt(range.from).to >= range.to);
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

/** Source indentation is structural; the rendered list uses a fixed step per level. */
function liveListLayout(node: SyntaxNodeRef, state: EditorState, out: LiveRange[]) {
  const item = node.node.parent;
  if (!item) return;
  const line = state.doc.lineAt(node.from);
  liveListLine(item, state, line, node.from, false, out);
  if (item.parent?.name === "OrderedList") {
    const spaces = /^[ \t]*/.exec(state.doc.sliceString(node.to, line.to))![0].length;
    out.push({ kind: "mark", from: node.from, to: node.to + spaces, className: "cm-live-list-marker" });
  }
}

function liveListLine(item: SyntaxNode, state: EditorState, line: { from: number; text: string }, contentFrom: number, continuation: boolean, out: LiveRange[]) {
  let depth = 0;
  for (let parent = item.parent?.parent; parent; parent = parent.parent) {
    if (parent.name === "ListItem") depth += 1;
  }
  const start = line.from + indentation(line.text).from;
  if (start < contentFrom) {
    out.push({ kind: "hide", from: start, to: contentFrom });
    out.push({ kind: "atomic", from: start, to: contentFrom });
  }
  const marker = item.getChild("ListMark")!;
  const width = item.parent?.name === "OrderedList" ? `max(1.5em, ${marker.to - marker.from + 1}ch)` : "1.5em";
  out.push({ kind: "line", from: line.from, className: `cm-live-list${continuation ? " cm-live-list-continuation" : ""}`, style: `--cm-list-depth: ${depth}; --cm-list-marker: ${width}` });
}

/** Hard breaks and later paragraphs get the same content column as the item's text. */
function liveListParagraph(node: SyntaxNodeRef, state: EditorState, out: LiveRange[], from: number, to: number) {
  let item = node.node.parent;
  while (item && item.name !== "ListItem") item = item.parent;
  if (!item) return;
  const doc = state.doc;
  const markerLine = doc.lineAt(item.from).number;
  const first = doc.lineAt(node.from).number;
  const joined = node.name === "Paragraph" && doc.lineAt(node.to).number - first <= MAX_JOINED_LINES;
  const last = doc.lineAt(Math.min(node.to, to)).number;
  for (let number = doc.lineAt(Math.max(node.from, from)).number; number <= last; number += 1) {
    if (number === markerLine) continue;
    if (number > first && joined) {
      const previous = doc.line(number - 1).text;
      if (!/ {2}$/.test(previous) && !(/\\*$/.exec(previous)![0].length % 2)) continue;
    }
    const line = doc.line(number);
    const indent = indentation(line.text);
    liveListLine(item, state, line, line.from + indent.from + indent.text.length, true, out);
  }
}

function liveListMark(node: SyntaxNodeRef, doc: Doc, out: LiveRange[], blockMark: (from: number, to: number, hide: boolean) => void) {
  const item = node.node.parent;
  // Ordered numbers stay text: they are what the reader sees in the preview too.
  if (item?.parent?.name !== "BulletList") return;
  // A task item is drawn as its checkbox alone.
  const task = !!item.getChild("Task");
  if (task) blockMark(node.from, node.to, true);
  else {
    const end = node.to + /^[ \t]*/.exec(doc.sliceString(node.to, doc.lineAt(node.to).to))![0].length;
    out.push({ kind: "bullet", from: node.from, to: end });
    out.push({ kind: "atomic", from: node.from, to: end });
  }
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
    span.className = "cm-live-bullet cm-live-list-marker";
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

const bullet = Decoration.replace({ widget: new BulletWidget() });
const rule = Decoration.replace({ widget: new RuleWidget() });
const hidden = Decoration.replace({});
const atomic = Decoration.mark({});

function toDecoration(range: Exclude<LiveRange, { kind: "atomic" }>, readOnly: boolean): Range<Decoration> {
  switch (range.kind) {
    case "hide": return hidden.range(range.from, range.to);
    case "line": return Decoration.line({ class: range.className, attributes: range.style ? { style: range.style } : undefined }).range(range.from);
    case "mark": return Decoration.mark({ class: range.className }).range(range.from, range.to);
    case "bullet": return bullet.range(range.from, range.to);
    case "rule": return rule.range(range.from, range.to);
    case "task": return Decoration.replace({ widget: new TaskWidget(range.checked, readOnly) }).range(range.from, range.to);
    case "image": return Decoration.replace({ widget: new ImageWidget(range.src, range.alt) }).range(range.from, range.to);
  }
}

function buildDecorations(view: EditorView): { decorations: DecorationSet; atomic: RangeSet<Decoration> } {
  const ranges: Range<Decoration>[] = [];
  const atomics: Range<Decoration>[] = [];
  for (const visible of view.visibleRanges) {
    for (const range of liveRanges(view.state, visible.from, visible.to)) {
      if ((range.kind === "hide" || range.kind === "atomic") && range.from === range.to) continue;
      if (range.kind === "atomic") atomics.push(atomic.range(range.from, range.to));
      else ranges.push(toDecoration(range, view.state.readOnly));
    }
  }
  return { decorations: Decoration.set(ranges, true), atomic: Decoration.set(atomics, true) };
}

const livePlugin = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  atomic: RangeSet<Decoration>;
  constructor(view: EditorView) { ({ decorations: this.decorations, atomic: this.atomic } = buildDecorations(view)); }
  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged || update.selectionSet
      || update.startState.readOnly !== update.state.readOnly
      || update.startState.field(editorFocused) !== update.state.field(editorFocused)
      || syntaxTree(update.startState) !== syntaxTree(update.state)) {
      ({ decorations: this.decorations, atomic: this.atomic } = buildDecorations(update.view));
    }
  }
}, { decorations: (plugin) => plugin.decorations });

const BLOCK_MARKS = new Set(["HeaderMark", "QuoteMark", "ListMark", "TaskMarker"]);

/**
 * Where the text of `line` starts, after its hidden block markers: `# `, `> `,
 * `- `, `- [ ] ` and any indentation before them. A caret before that point
 * would sit on markup the user cannot see, and typing there would break it.
 */
export function blockMarkupEnd(state: EditorState, line: { from: number; to: number }, hiddenOnly = false): number {
  const doc = state.doc;
  let end = line.from;
  syntaxTree(state).iterate({
    from: line.from,
    to: line.to,
    enter: (node) => {
      if (node.from > line.to || node.from < end) return node.from > line.to ? false : undefined;
      if (!BLOCK_MARKS.has(node.name)) return undefined;
      if (doc.sliceString(end, node.from).trim()) return false;
      if (node.name === "HeaderMark" && node.node.parent?.name.startsWith("Setext")) return undefined;
      if (hiddenOnly && node.name === "HeaderMark" && node.node.parent
        && touches(activeRanges(state), node.node.parent.from, node.node.parent.to)) return undefined;
      // Ordered numbers are shown, and so editable.
      if (node.name === "ListMark" && node.node.parent?.parent?.name !== "BulletList") {
        if (hiddenOnly) end = node.from;
        return undefined;
      }
      const spaces = node.name === "ListMark" || node.name === "TaskMarker"
        ? /^[ \t]*/.exec(doc.sliceString(node.to, line.to))![0].length
        : (doc.sliceString(node.to, node.to + 1) === " " ? 1 : 0);
      end = Math.min(node.to + spaces, line.to);
      return undefined;
    },
  });
  return end;
}

/** Keeps an empty caret out of hidden block markup: a click, Home or a vertical move lands at the line's text. */
const caretOutOfMarkup = EditorState.transactionFilter.of((transaction) => {
  if (!transaction.selection) return transaction;
  const state = transaction.state;
  let moved = false;
  const ranges = transaction.selection.ranges.map((range) => {
    if (!range.empty) return range;
    const end = blockMarkupEnd(state, state.doc.lineAt(range.head), true);
    if (range.head >= end) return range;
    moved = true;
    return EditorSelection.cursor(end);
  });
  if (!moved) return transaction;
  return [transaction, { selection: EditorSelection.create(ranges, transaction.selection.mainIndex), sequential: true }];
});

/** Left from the start of a line's text goes on to the end of the line above, past the hidden markup. */
const leftOverMarkup: Command = (view) => {
  const range = view.state.selection.main;
  if (view.state.selection.ranges.length > 1 || !range.empty) return false;
  const line = view.state.doc.lineAt(range.head);
  if (line.from === 0 || range.head === line.from || range.head !== blockMarkupEnd(view.state, line, true)) return false;
  view.dispatch({ selection: { anchor: line.from - 1 }, scrollIntoView: true, userEvent: "select" });
  return true;
};

/** The block containers a table can sit inside. Everything else is skipped whole. */
const TABLE_CONTAINERS = new Set(["Document", "Blockquote", "BulletList", "OrderedList", "ListItem"]);

/** The tables overlapping `[from, to]`, each as its whole source. */
export function tableRanges(state: EditorState, from = 0, to = state.doc.length): Array<{ from: number; to: number; source: string }> {
  const out: Array<{ from: number; to: number; source: string }> = [];
  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      if (node.name === "Table") {
        const start = state.doc.lineAt(node.from).from;
        const end = state.doc.lineAt(node.to).to;
        // A table inside a quote or a list would take the markers on its lines
        // with it, so only one that starts its line, after any indentation, is drawn whole.
        if (!state.doc.sliceString(start, node.from).trim()) out.push({ from: start, to: end, source: state.doc.sliceString(start, end) });
        return false;
      }
      return TABLE_CONTAINERS.has(node.name);
    },
  });
  return out;
}

/**
 * Whether an edit can change how far-off lines parse: only a code fence or an
 * HTML block opening or closing can swallow, or release, everything after it.
 * Redrawing to the end of the document on every keystroke instead cost
 * milliseconds per character on a large file.
 */
function mayReflowBlocks(transaction: Transaction): boolean {
  let found = false;
  const marks = (doc: Text, from: number, to: number) => doc.sliceString(doc.lineAt(from).from, doc.lineAt(to).to).match(REFLOW)?.join("\n") ?? "";
  // Compared, not just tested: typing a fence's language name or inside an
  // HTML line keeps the same openers and closers, and changes nothing far off.
  transaction.changes.iterChangedRanges((fromA, toA, fromB, toB) => {
    found ||= marks(transaction.startState.doc, fromA, toA) !== marks(transaction.state.doc, fromB, toB);
  });
  return found;
}

/** Fence and HTML block openers at the start of a line, and the HTML block closers that end one anywhere on it. */
const REFLOW = /^ {0,3}(?:`{3,}|~{3,}|<[!?/]?[\w-]*)|-->|\?>|\]\]>|<\/(?:script|pre|style|textarea)>/gim;

function tableDecoration(table: { from: number; to: number; source: string }, readOnly: boolean): Range<Decoration> {
  return Decoration.replace({ widget: new TableWidget(table.source, readOnly), block: true }).range(table.from, table.to);
}

/** What a finder draws for one block: its decorations, and the stretch they belong to. */
interface Drawn { from: number; to: number; decorations: Range<Decoration>[] }

/**
 * Redraws the blocks in each stretch, leaving the rest of the set as it was.
 *
 * Each stretch is widened to whole lines and one character either side, so a
 * line typed directly above or below a block still reaches it, and then to the
 * full extent of every block found, so a block is always redrawn whole.
 */
function refreshStretches(value: DecorationSet, state: EditorState, stretches: ReadonlyArray<readonly [number, number]>, find: (state: EditorState, from: number, to: number) => Drawn[]): DecorationSet {
  const doc = state.doc;
  for (const [start, end] of stretches) {
    const from = Math.max(0, doc.lineAt(Math.min(start, doc.length)).from - 1);
    const to = Math.min(doc.length, doc.lineAt(Math.min(end, doc.length)).to + 1);
    const found = find(state, from, to);
    let filterFrom = from;
    let filterTo = to;
    for (const block of found) {
      filterFrom = Math.min(filterFrom, block.from);
      filterTo = Math.max(filterTo, block.to);
    }
    value = value.update({ filterFrom, filterTo, filter: () => false, add: found.flatMap((block) => block.decorations), sort: true });
  }
  return value;
}

/**
 * Decorations for whole blocks, which may span lines and so cannot come from
 * the view plugin.
 *
 * Kept up to date by stretch rather than rebuilt. A full rebuild walks the
 * block structure of the entire document on every keystroke; on a 2 MB file
 * that was most of the cost of typing a character. A block's face changes with
 * an edit near it, the parser reaching it, or the document's writability.
 */
function drawnBlocks(find: (state: EditorState, from: number, to: number) => Drawn[]) {
  const all = (state: EditorState) => RangeSet.of(find(state, 0, state.doc.length).flatMap((block) => block.decorations), true);
  return StateField.define<DecorationSet>({
    create: all,
    update: (value, transaction) => {
      const before = transaction.startState;
      const after = transaction.state;
      if (before.readOnly !== after.readOnly) return all(after);
      const stretches: Array<readonly [number, number]> = [];
      if (transaction.docChanged) {
        value = value.map(transaction.changes);
        let first = after.doc.length;
        transaction.changes.iterChangedRanges((_fromA, _toA, fromB, toB) => {
          stretches.push([fromB, toB]);
          first = Math.min(first, fromB);
        });
        // A fence or an HTML block opening or closing can swallow, or release,
        // every block after it: those are redrawn up to where the parser has reached.
        if (mayReflowBlocks(transaction)) stretches.push([first, Math.max(first, Math.min(syntaxTree(after).length, after.doc.length))]);
      }
      const treeBefore = syntaxTree(before);
      const treeAfter = syntaxTree(after);
      // The parser working through the document in the background: the newly
      // parsed stretch may hold blocks nobody has drawn yet.
      if (!transaction.docChanged && treeBefore !== treeAfter) {
        stretches.push([Math.min(treeBefore.length, treeAfter.length), treeAfter.length]);
      }
      return stretches.length ? refreshStretches(value, after, stretches, find) : value;
    },
    provide: (field) => EditorView.decorations.from(field),
  });
}

/** Every table, drawn whole and edited in place. */
const tableField = drawnBlocks((state, from, to) => tableRanges(state, from, to)
  .map((table) => ({ from: table.from, to: table.to, decorations: [tableDecoration(table, state.readOnly)] })));

class SpaceWidget extends WidgetType {
  eq() { return true; }
  toDOM() {
    const span = document.createElement("span");
    span.textContent = " ";
    return span;
  }
}

const softBreak = Decoration.replace({ widget: new SpaceWidget() });

/**
 * The line breaks inside the paragraphs overlapping `[from, to]`, each with
 * the next line's indentation: a soft break, which Markdown reads as a space.
 * A break after two spaces or a backslash is a hard one and stays.
 */
const MAX_JOINED_LINES = 300;

export function softBreaks(state: EditorState, from = 0, to = state.doc.length): Array<{ from: number; to: number }> {
  const doc = state.doc;
  const out: Array<{ from: number; to: number }> = [];
  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      if (node.name !== "Paragraph") return TABLE_CONTAINERS.has(node.name);
      // A paragraph of hundreds of lines drawn as one line would defeat the
      // editor drawing only what is on screen; it keeps its lines.
      if (doc.lineAt(node.to).number - doc.lineAt(node.from).number > MAX_JOINED_LINES) return false;
      for (let line = doc.lineAt(node.from); line.to < node.to; line = doc.line(line.number + 1)) {
        // Two trailing spaces, or an odd run of backslashes, make a hard break.
        const spaces = /[ ]*$/.exec(line.text)![0].length;
        if (spaces >= 2 || /\\*$/.exec(line.text)![0].length % 2) continue;
        const next = doc.line(line.number + 1);
        const indent = /^[ \t]*/.exec(next.text)![0].length;
        out.push({ from: line.to - spaces, to: next.from + indent });
      }
      return false;
    },
  });
  return out;
}

/**
 * A paragraph's lines drawn as one flowing paragraph, as the preview draws
 * them: a hard-wrapped file fills the width instead of stopping where the
 * file's lines end. The breaks stay in the file; each is kept whole, so
 * Backspace after one joins the lines in the source too.
 */
const softBreakField = drawnBlocks((state, from, to) => {
  const drawn: Drawn[] = [];
  syntaxTree(state).iterate({
    from,
    to,
    enter: (node) => {
      if (TABLE_CONTAINERS.has(node.name)) return true;
      // Any other block claims its extent too, so a paragraph that an edit
      // below turned into a table or a heading loses its joins.
      if (node.name !== "Paragraph") {
        drawn.push({ from: node.from, to: node.to, decorations: [] });
        return false;
      }
      const breaks = softBreaks(state, node.from, node.to);
      if (breaks.length) drawn.push({ from: node.from, to: node.to, decorations: breaks.map((range) => softBreak.range(range.from, range.to)) });
      return false;
    },
  });
  return drawn;
});

/** The table drawn on the line after (`forward`) or before the caret's, if the caret is about to cross it. */
function tableNext(view: EditorView, forward: boolean, vertical: boolean): number | undefined {
  const range = view.state.selection.main;
  if (!range.empty || view.state.readOnly) return undefined;
  const line = view.state.doc.lineAt(range.head);
  let found: number | undefined;
  if (vertical) {
    // Where the move would land, not the source line: a paragraph's lines are
    // drawn as one, and a wrapped one moves within itself first.
    const moved = view.moveVertically(range, forward).head;
    view.state.field(tableField).between(Math.min(range.head, moved), Math.max(range.head, moved), (from, to) => {
      if (forward ? from > range.head && from <= moved && found === undefined : to < range.head && to >= moved) found = from;
    });
    return found;
  }
  const target = forward ? line.to + 1 : line.from - 1;
  if (target < 0 || target > view.state.doc.length) return undefined;
  if (range.head !== (forward ? line.to : line.from)) return undefined;
  view.state.field(tableField).between(target, target, (from, to) => {
    if (forward ? from === target : to === target) found = from;
  });
  return found;
}

function intoTable(forward: boolean, vertical: boolean): Command {
  return (view) => {
    const at = tableNext(view, forward, vertical);
    return at !== undefined && enterTable(view, at, forward);
  };
}

/**
 * Enter at the start of a heading's text opens a line above it, as in Typora,
 * rather than splitting the hidden `#` from its text.
 */
export const breakBeforeHeading: Command = (view) => {
  const { state } = view;
  const range = state.selection.main;
  if (state.selection.ranges.length > 1 || !range.empty || state.readOnly) return false;
  const line = state.doc.lineAt(range.head);
  if (range.head === line.from || range.head !== blockMarkupEnd(state, line) || !/^ATXHeading/.test(syntaxTree(state).resolveInner(line.from, 1).parent?.name ?? "")) return false;
  view.dispatch({ changes: { from: line.from, insert: state.lineBreak }, selection: { anchor: range.head + state.lineBreak.length }, scrollIntoView: true, userEvent: "input" });
  return true;
};

const liveKeys = Prec.high(keymap.of([
  { key: "Enter", run: breakBeforeHeading },
  { key: "Tab", run: (view) => changeListIndent(view, 1), shift: (view) => changeListIndent(view, -1) },
  { key: "Mod-]", run: (view) => changeListIndent(view, 1) },
  { key: "Mod-[", run: (view) => changeListIndent(view, -1) },
  { key: "ArrowDown", run: intoTable(true, true) },
  { key: "ArrowUp", run: intoTable(false, true) },
  { key: "ArrowRight", run: intoTable(true, false) },
  { key: "ArrowLeft", run: (view) => intoTable(false, false)(view) || leftOverMarkup(view) },
]));

const liveHighlight = HighlightStyle.define([
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strong, fontWeight: "650" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.heading, fontWeight: "650", color: "var(--chrome-ink-strong)" },
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

function inCode(state: EditorState, pos: number, includeInline = true): boolean {
  for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, -1); node; node = node.parent) {
    if (node.name === "FencedCode" || node.name === "CodeBlock" || (includeInline && node.name === "InlineCode")) return true;
  }
  return false;
}

function enclosing(state: EditorState, pos: number, name: string): SyntaxNode | undefined {
  for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, -1); node; node = node.parent) {
    if (node.name === name) return node;
  }
  return undefined;
}

/** The indentation inside any explicit quote prefix, keeping the quote itself intact. */
function indentation(text: string, quoteDepth = Infinity): { from: number; text: string } {
  let from = 0;
  for (let depth = 0; depth < quoteDepth; depth += 1) {
    const quote = /^[ \t]*>[ \t]?/.exec(text.slice(from));
    if (!quote) break;
    from += quote[0].length;
  }
  return { from, text: /^[ \t]*/.exec(text.slice(from))![0] };
}

function enclosingQuotes(item: SyntaxNode): number {
  let depth = 0;
  for (let parent = item.parent; parent; parent = parent.parent) {
    if (parent.name === "Blockquote") depth += 1;
  }
  return depth;
}

/** Move whole list items, including continuation lines and descendants, in one edit. */
export function changeListIndent(view: EditorView, direction: 1 | -1): boolean {
  const { state } = view;
  if (state.readOnly) return false;
  const items = new Map<number, SyntaxNode>();
  for (const range of state.selection.ranges) {
    if (range.empty && inCode(state, range.head, false)) return false;
    const last = state.doc.lineAt(range.to > range.from ? range.to - 1 : range.to).number;
    for (let number = state.doc.lineAt(range.from).number; number <= last; number += 1) {
      const line = state.doc.line(number);
      if (!line.text.trim()) continue;
      const indent = indentation(line.text);
      const pos = Math.max(line.from + indent.from + indent.text.length, Math.min(range.from, line.to));
      let item: SyntaxNode | undefined;
      for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(pos, pos === line.to ? -1 : 1); node; node = node.parent) {
        if (node.name === "ListItem") { item = node; break; }
      }
      if (!item) return false;
      items.set(item.from, item);
    }
  }
  if (!items.size) return false;
  const selected = [...items.values()].sort((a, b) => a.from - b.from);
  const roots: SyntaxNode[] = [];
  for (const item of selected) {
    if (!roots.length || roots[roots.length - 1].to < item.to) roots.push(item);
  }
  const indentColumn = (text: string, quoteDepth: number) => {
    const indent = indentation(text, quoteDepth);
    const prefix = text.slice(0, indent.from);
    return countColumn(prefix + indent.text, state.tabSize) - countColumn(prefix, state.tabSize);
  };
  const column = (item: SyntaxNode) => indentColumn(state.doc.lineAt(item.from).text, enclosingQuotes(item));
  const changes: ChangeSpec[] = [];
  // Adjacent selected siblings move together beneath the sibling before the group.
  let group: { list: number; end: number; delta: number } | undefined;
  for (const item of roots) {
    let previous = item.prevSibling;
    // Explicit quote marks may sit between siblings in the syntax tree.
    while (previous && previous.name !== "ListItem") previous = previous.prevSibling;
    let delta = 0;
    if (group && group.list === item.parent?.from && previous?.from === group.end) delta = group.delta;
    else if (direction === 1) {
      if (previous?.name === "ListItem") {
        const marker = previous.getChild("ListMark")!;
        const line = state.doc.lineAt(previous.from);
        const gap = /^[ \t]*/.exec(state.doc.sliceString(marker.to, line.to))![0];
        const width = marker.to - marker.from;
        const prefix = state.doc.sliceString(line.from, marker.to);
        const padding = countColumn(prefix + gap, state.tabSize) - countColumn(prefix, state.tabSize);
        // Five or more spaces start code inside the item after one separator.
        delta = column(previous) + width + (padding > 4 ? 1 : Math.max(1, padding)) - column(item);
      }
    } else {
      const parent = item.parent?.parent;
      if (parent?.name === "ListItem") delta = column(parent) - column(item);
    }
    group = { list: item.parent!.from, end: item.from, delta };
    if (!delta) continue;
    const quoteDepth = enclosingQuotes(item);
    const last = state.doc.lineAt(item.to).number;
    for (let number = state.doc.lineAt(item.from).number; number <= last; number += 1) {
      const line = state.doc.line(number);
      if (!line.text.trim()) continue;
      const indent = indentation(line.text, quoteDepth);
      const from = line.from + indent.from;
      changes.push({ from, to: from + indent.text.length, insert: " ".repeat(Math.max(0, indentColumn(line.text, quoteDepth) + delta)) });
    }
  }
  if (changes.length) {
    const changeSet = state.changes(changes);
    view.dispatch({ changes: changeSet, selection: state.selection.map(changeSet), scrollIntoView: true, userEvent: direction === 1 ? "input.indent" : "delete.dedent" });
  }
  return true;
}

function nextItem(item: RegExpExecArray): string {
  const number = item[3] ? `${Number(item[3]) + 1}${item[4]}` : item[2];
  return `${item[1]}${number}${item[5]}${item[6] ? "[ ] " : ""}`;
}

/**
 * Enter starts the next block, as in Typora: the next item in a list, a new
 * paragraph in a quote or in the text. A single line break would only be
 * drawn as a space — a paragraph's lines flow together — so the line typed
 * after it would join the paragraph above. Enter on an empty item ends the
 * list.
 */
export const continueMarkup: Command = (view) => {
  const { state } = view;
  const range = state.selection.main;
  if (state.selection.ranges.length > 1 || !range.empty || state.readOnly) return false;
  const line = state.doc.lineAt(range.head);
  if (inCode(state, range.head)) return false;
  const br = state.lineBreak;
  const insert = (text: string) => view.dispatch(state.update(state.replaceSelection(text), { scrollIntoView: true, userEvent: "input" }));
  // The parser's word, not the pattern's: `* * *` matches but is a rule.
  const candidate = LIST_ITEM.exec(line.text);
  const item = candidate && syntaxTree(state).resolveInner(line.from + candidate[1].length, 1).name === "ListMark" ? candidate : undefined;
  const quote = item ? undefined : QUOTE_ONLY.exec(line.text);
  const marker = item ?? quote;
  if (marker) {
    if (range.head < line.from + marker[0].length) return false;
    if (!line.text.slice(marker[0].length).trim() && range.head === line.to) {
      // An empty item: the user is done with the list.
      const prefix = item ? item[1] : "";
      view.dispatch({ changes: { from: line.from, to: line.to, insert: prefix }, selection: { anchor: line.from + prefix.length }, userEvent: "input" });
      return true;
    }
    if (item) insert(`${br}${nextItem(item)}`);
    // A blank quote line between, or the new line would continue the paragraph.
    else insert(`${br}${marker[0].trimEnd()}${br}${marker[0]}`);
    return true;
  }
  const paragraph = enclosing(state, range.head, "Paragraph");
  if (!paragraph) return false;
  // A wrapped item's later line: the next item takes the marker of the item's first line.
  const listItem = paragraph.parent?.name === "ListItem" ? paragraph.parent : undefined;
  const first = listItem && LIST_ITEM.exec(state.doc.lineAt(listItem.from).text);
  insert(first ? `${br}${nextItem(first)}` : `${br}${br}`);
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
    EditorView.atomicRanges.of((view) => view.plugin(livePlugin)?.atomic ?? RangeSet.empty),
    caretOutOfMarkup,
    tableField,
    softBreakField,
    EditorView.atomicRanges.of((view) => view.state.field(softBreakField)),
    liveKeys,
    linkHandler.of(options.onLink),
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
