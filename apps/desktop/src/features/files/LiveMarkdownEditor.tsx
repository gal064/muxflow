import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { Annotation, Compartment, EditorSelection, EditorState, Prec, Transaction } from "@codemirror/state";
import { EditorView, drawSelection, keymap } from "@codemirror/view";
import { useEffect, useRef } from "react";
import { continueMarkup, liveMarkdown, toggleInline } from "./liveMarkdown";
import { searchCount } from "./searchCount";

export interface LiveMarkdownEditorProps {
  /** Stable identity of the buffer, so the caret and scroll survive a mode switch. */
  modelPath: string;
  value: string;
  readOnly: boolean;
  onChange(content: string): void;
  /** The editor exists and is about to paint. */
  onReady(): void;
  /** A link was activated with ⌘ (Ctrl on Linux) held. */
  onLink(href: string): void;
}

/** A change that came from the buffer, not the user; it must not echo back as an edit. */
const fromBuffer = Annotation.define<boolean>();

/** Where each buffer was left, for the next time it is shown in this mode. */
const viewStates = new Map<string, { anchor: number; head: number; scrollTop: number }>();

/**
 * The strings this editor has handed to `onChange` that have not come back as
 * `value` yet.
 *
 * `value` lags the document: a second keystroke can land before the render
 * carrying the first one, and that render must not be mistaken for the file
 * changing underneath — applying it would delete the second keystroke. Echoes
 * arrive in the order they were sent, so everything up to a recognised one is
 * settled with it.
 */
export class PendingEchoes {
  #sent: string[] = [];

  sent(content: string): void {
    this.#sent.push(content);
    if (this.#sent.length > 64) this.#sent.shift();
  }

  /** True when `value` is one of ours. Anything else clears the queue. */
  settle(value: string): boolean {
    const index = this.#sent.indexOf(value);
    this.#sent = index < 0 ? [] : this.#sent.slice(index + 1);
    return index >= 0;
  }
}

/** The smallest replacement that turns `before` into `after`, so the caret and undo history survive it. */
export function minimalChange(before: string, after: string): { from: number; to: number; insert: string } {
  let start = 0;
  const shortest = Math.min(before.length, after.length);
  while (start < shortest && before.charCodeAt(start) === after.charCodeAt(start)) start += 1;
  let end = 0;
  while (end < shortest - start
    && before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)) end += 1;
  return { from: start, to: before.length - end, insert: after.slice(start, after.length - end) };
}

function editable(readOnly: boolean) {
  return [EditorState.readOnly.of(readOnly)];
}

/**
 * Markdown edited in its rendered form, and the only module that imports
 * CodeMirror. Like `FileEditor`, it is its own chunk: no other mode fetches it.
 *
 * The parent remounts it per buffer (`key`), so the document it was created
 * with is the document it edits.
 */
export function LiveMarkdownEditor(props: LiveMarkdownEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>(undefined);
  const echoes = useRef(new PendingEchoes());
  const readOnly = useRef(new Compartment());
  // Read from CodeMirror's callbacks, which outlive the render that created them.
  const latest = useRef(props);
  useEffect(() => { latest.current = props; });

  useEffect(() => {
    const parent = host.current;
    if (!parent) return;
    const initial = latest.current;
    const saved = viewStates.get(initial.modelPath);
    const length = initial.value.length;
    const created = new EditorView({
      parent,
      state: EditorState.create({
        doc: initial.value,
        selection: saved ? EditorSelection.single(Math.min(saved.anchor, length), Math.min(saved.head, length)) : undefined,
        extensions: [
          liveMarkdown({ onLink: (href) => latest.current.onLink(href) }),
          history(),
          // Draw from CodeMirror's document positions, including widget edges
          // where WebKit's native caret can disappear or sit inside a marker.
          drawSelection(),
          search({ top: true }),
          searchCount,
          highlightSelectionMatches(),
          readOnly.current.of(editable(initial.readOnly)),
          Prec.high(keymap.of([
            { key: "Enter", run: continueMarkup },
            { key: "Mod-b", run: toggleInline("**") },
            { key: "Mod-i", run: toggleInline("*") },
          ])),
          keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
          EditorView.contentAttributes.of({ "aria-label": "Markdown document", spellcheck: "true" }),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged || update.transactions.every((transaction) => transaction.annotation(fromBuffer))) return;
            const content = update.state.doc.toString();
            echoes.current.sent(content);
            latest.current.onChange(content);
          }),
        ],
      }),
    });
    view.current = created;
    if (saved) created.scrollDOM.scrollTop = saved.scrollTop;
    initial.onReady();
    return () => {
      const { anchor, head } = created.state.selection.main;
      viewStates.set(initial.modelPath, { anchor, head, scrollTop: created.scrollDOM.scrollTop });
      created.destroy();
      view.current = undefined;
    };
  }, []);

  useEffect(() => {
    const current = view.current;
    if (!current || echoes.current.settle(props.value)) return;
    // The document holds `\n` whatever the file uses; autosave restores the
    // file's own line ending on write. Compared raw, a CRLF file differs on
    // every line and the replacement would insert stray breaks.
    const next = props.value.replace(/\r\n?/g, "\n");
    const text = current.state.doc.toString();
    if (text === next) return;
    // Out of the undo history: undoing past a reload would resurrect content
    // the file no longer has, and autosave would write it straight back.
    current.dispatch({
      changes: minimalChange(text, next),
      annotations: [fromBuffer.of(true), Transaction.addToHistory.of(false)],
    });
  }, [props.value]);

  useEffect(() => {
    view.current?.dispatch({ effects: readOnly.current.reconfigure(editable(props.readOnly)) });
  }, [props.readOnly]);

  return <div className="live-markdown" ref={host} />;
}
