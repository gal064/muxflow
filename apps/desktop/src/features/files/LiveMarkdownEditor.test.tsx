// @vitest-environment jsdom
// jsdom, because CodeMirror mounts into a real DOM.
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LiveMarkdownEditor, PendingEchoes, minimalChange } from "./LiveMarkdownEditor";
import { EditorView } from "@codemirror/view";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("minimalChange", () => {
  it("replaces only what differs", () => {
    expect(minimalChange("hello world", "hello brave world")).toEqual({ from: 6, to: 6, insert: "brave " });
    expect(minimalChange("aaa", "aa")).toEqual({ from: 2, to: 3, insert: "" });
    expect(minimalChange("same", "same")).toEqual({ from: 4, to: 4, insert: "" });
    expect(minimalChange("", "new")).toEqual({ from: 0, to: 0, insert: "new" });
  });
});

describe("PendingEchoes", () => {
  it("recognises a lagging echo and settles everything sent before it", () => {
    const echoes = new PendingEchoes();
    echoes.sent("a");
    echoes.sent("ab");
    expect(echoes.settle("a")).toBe(true);
    expect(echoes.settle("ab")).toBe(true);
    expect(echoes.settle("ab")).toBe(false);
  });

  it("treats anything else as the file changing", () => {
    const echoes = new PendingEchoes();
    echoes.sent("mine");
    expect(echoes.settle("theirs")).toBe(false);
    expect(echoes.settle("mine")).toBe(false);
  });
});

describe("LiveMarkdownEditor", () => {
  let root: Root | undefined;
  let host: HTMLDivElement | undefined;
  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    root = undefined;
  });

  function mount(value: string, onChange = vi.fn()) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const props = { modelPath: `test://${Math.random()}`, readOnly: false, onChange, onReady: vi.fn(), onLink: vi.fn() };
    act(() => root!.render(<LiveMarkdownEditor {...props} value={value} />));
    const editor = EditorView.findFromDOM(host.querySelector<HTMLElement>(".cm-editor")!)!;
    return { props, editor, rerender: (next: string) => act(() => root!.render(<LiveMarkdownEditor {...props} value={next} />)) };
  }

  it("never writes a document the user has not edited", () => {
    const source = "*   odd   bullets\n\n__strong__ and  | a |b|\n|-|-|\n";
    const { props, editor } = mount(source);
    expect(props.onReady).toHaveBeenCalledTimes(1);
    expect(editor.state.doc.toString()).toBe(source);
    expect(props.onChange).not.toHaveBeenCalled();
  });

  it("does not undo a keystroke when the render carrying the previous one arrives late", () => {
    const { props, editor, rerender } = mount("ab");
    act(() => editor.dispatch({ changes: { from: 2, insert: "c" } }));
    act(() => editor.dispatch({ changes: { from: 3, insert: "d" } }));
    expect(props.onChange).toHaveBeenLastCalledWith("abcd");
    rerender("abc");
    expect(editor.state.doc.toString()).toBe("abcd");
    rerender("abcd");
    expect(editor.state.doc.toString()).toBe("abcd");
  });

  it("edits a CRLF file without inventing lines", () => {
    const { props, editor, rerender } = mount("# a\r\nb\r\nc\r\n");
    expect(editor.state.doc.toString()).toBe("# a\nb\nc\n");
    act(() => editor.dispatch({ changes: { from: 3, insert: "!" } }));
    expect(props.onChange).toHaveBeenLastCalledWith("# a!\nb\nc\n");
    rerender("# a!\r\nb\r\nc\r\nd\r\n");
    expect(editor.state.doc.toString()).toBe("# a!\nb\nc\nd\n");
  });

  it("takes an outside change in place, without echoing it as an edit", () => {
    const { props, editor, rerender } = mount("one\ntwo\n");
    act(() => editor.dispatch({ selection: { anchor: 6 } }));
    rerender("zero\none\ntwo\n");
    expect(editor.state.doc.toString()).toBe("zero\none\ntwo\n");
    expect(editor.state.selection.main.head).toBe(11);
    expect(props.onChange).not.toHaveBeenCalled();
  });
});
