// @vitest-environment jsdom
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, expect, it, vi } from "vitest";
import { liveMarkdown } from "./liveMarkdown";

// CodeMirror selects its engine path at module load, before creating any view.
vi.hoisted(() => {
  Object.defineProperty(document.documentElement.style, "webkitFontSmoothing", { configurable: true, value: "" });
  Object.defineProperty(navigator, "platform", { configurable: true, get: () => "Linux x86_64" });
  Object.defineProperty(navigator, "userAgent", { configurable: true, get: () => "Mozilla/5.0 AppleWebKit/605.1.15" });
});

let view: EditorView | undefined;
afterEach(() => view?.destroy());

it.each(["- ", "1. "])("retains native typing inside a GTK caret buffer after %s", (marker) => {
  const doc = `Unicode 😀 café\n\n${marker}`;
  const onChange = vi.fn();
  view = new EditorView({ parent: document.body, state: EditorState.create({
    doc, selection: { anchor: doc.length }, extensions: [liveMarkdown({ onLink() {} }),
      EditorView.updateListener.of((update) => { if (update.docChanged) onChange(update.state.doc.toString()); })],
  }) });
  view.focus();
  expect(view.contentDOM.querySelector("img.cm-widgetBuffer")).toBeNull();
  const insert = (text: string) => {
    const buffer = view!.contentDOM.querySelector(".cm-line:last-child .cm-widgetBuffer:last-child")!;
    expect(buffer.nodeName).toBe("SPAN");
    const node = document.createTextNode(text);
    buffer.append(node);
    document.getSelection()!.setBaseAndExtent(node, text.length, node, text.length);
    // Flush the real DOM observer, as a browser input frame would. This tests
    // reading a native DOM edit rather than dispatching a document change.
    (view as unknown as { observer: { flush(): void } }).observer.flush();
  };
  insert("z");
  expect(view.state.doc.toString()).toBe(`${doc}z`);
  expect(view.state.selection.main.head).toBe(doc.length + 1);
  expect(onChange).toHaveBeenLastCalledWith(`${doc}z`);
  const selection = document.getSelection()!;
  expect(view.posAtDOM(selection.focusNode!, selection.focusOffset)).toBe(doc.length + 1);
  expect([...view.contentDOM.querySelectorAll(".cm-widgetBuffer")].every((buffer) => !buffer.textContent)).toBe(true);
});
