// @vitest-environment jsdom
import { act, create } from "react-test-renderer";
import { describe, expect, it, vi } from "vitest";
import { attachEditorLayout, useEditorLayout, type LayoutableEditor } from "./editorLayout";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function editorIn(box: { width: number; height: number }) {
  const host = document.createElement("div");
  const node = document.createElement("div");
  host.append(node);
  host.getBoundingClientRect = () => ({ width: box.width, height: box.height, top: 0, left: 0, right: box.width, bottom: box.height, x: 0, y: 0, toJSON: () => ({}) });
  const layout = vi.fn();
  const editor: LayoutableEditor = { layout, getContainerDomNode: () => node };
  return { editor, layout, host, box };
}

describe("attachEditorLayout", () => {
  it("sizes the editor to its host instead of waiting to be told", () => {
    // The defect: `@monaco-editor/react` creates the editor while its container
    // is still `display: none`, so Monaco measures nothing, and the first resize
    // is not always delivered. Measured on the packaged app, the editor stayed
    // 30x157 inside a 740x690 pane — no text, and nothing to scroll.
    const { editor, layout } = editorIn({ width: 740, height: 690 });
    attachEditorLayout(editor, () => () => undefined);
    expect(layout).toHaveBeenCalledWith({ width: 740, height: 690 });
  });

  it("re-lays out when the host box changes, and stops when detached", () => {
    const { editor, layout, box } = editorIn({ width: 740, height: 690 });
    let notify = () => undefined as void;
    const disconnect = vi.fn();
    const detach = attachEditorLayout(editor, (_target, callback) => { notify = callback; return disconnect; });
    box.width = 400;
    notify();
    expect(layout).toHaveBeenLastCalledWith({ width: 400, height: 690 });
    detach();
    expect(disconnect).toHaveBeenCalled();
  });

  it("keeps the last good size when the pane is hidden rather than laying out at zero", () => {
    // A hidden tab measures 0x0. Laying out there would throw the size away and
    // bring the blank editor back when the tab is shown again — the defect, via
    // the fix.
    const { editor, layout, box } = editorIn({ width: 740, height: 690 });
    let notify = () => undefined as void;
    attachEditorLayout(editor, (_target, callback) => { notify = callback; return () => undefined; });
    layout.mockClear();
    box.width = 0;
    box.height = 0;
    notify();
    expect(layout).not.toHaveBeenCalled();
  });

  it("restores the size when a hidden pane is shown again", () => {
    // The other half of the hidden-pane rule: declining to lay out at zero is
    // only right if the box coming back is laid out at its new size.
    const { editor, layout, box } = editorIn({ width: 740, height: 690 });
    let notify = () => undefined as void;
    attachEditorLayout(editor, (_target, callback) => { notify = callback; return () => undefined; });
    box.width = 0;
    box.height = 0;
    notify();
    layout.mockClear();
    box.width = 900;
    box.height = 500;
    notify();
    expect(layout).toHaveBeenCalledWith({ width: 900, height: 500 });
  });

  it("still lays out once when there is no host to measure", () => {
    const layout = vi.fn();
    attachEditorLayout({ layout }, () => () => undefined);
    expect(layout).toHaveBeenCalledWith();
  });
});

describe("useEditorLayout", () => {
  /**
   * The observer's lifetime is the editor's, not the tab's.
   *
   * Both surfaces used to detach only when the whole tab lifecycle ended, so an
   * editor that came and went inside one tab — a Markdown view switched to
   * preview and back — left an observer attached to a box whose editor no
   * longer existed, once per switch.
   */
  it("detaches when the editor component unmounts", async () => {
    const observed: Element[] = [];
    const disconnect = vi.fn();
    const originalObserver = globalThis.ResizeObserver;
    Object.defineProperty(globalThis, "ResizeObserver", {
      configurable: true,
      value: class {
        observe(target: Element) { observed.push(target); }
        disconnect() { disconnect(); }
      },
    });
    try {
      const host = document.createElement("div");
      const node = document.createElement("div");
      host.append(node);
      const editor: LayoutableEditor = { layout: vi.fn(), getContainerDomNode: () => node };
      function Editor() {
        const attach = useEditorLayout();
        attach(editor);
        return null;
      }
      let renderer!: ReturnType<typeof create>;
      await act(async () => { renderer = create(<Editor />); });
      expect(observed).toEqual([host]);
      await act(async () => { renderer.unmount(); });
      expect(disconnect, "the editor went away and its observer stayed").toHaveBeenCalledTimes(1);
    } finally {
      if (originalObserver) Object.defineProperty(globalThis, "ResizeObserver", { configurable: true, value: originalObserver });
      else Reflect.deleteProperty(globalThis, "ResizeObserver");
    }
  });
});
