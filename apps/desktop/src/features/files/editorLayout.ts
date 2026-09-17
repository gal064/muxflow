/**
 * Keeps a Monaco editor the size of the box it was put in.
 *
 * Monaco's own `automaticLayout` is supposed to do this, and mostly does. What
 * it does not survive is the way `@monaco-editor/react` creates the editor: the
 * container it hands Monaco is still `display: none` at that moment (the wrapper
 * hides it until `isEditorReady` flips), so Monaco measures nothing, and the
 * observer that should notice the box appearing does not always deliver that
 * first transition in WKWebView. The editor is then laid out at its minimum
 * forever — measured on the packaged app at **30×157 px inside a 740×690 pane**,
 * which renders no text at all and, because `scrollBeyondLastLine` is off,
 * leaves nothing to scroll: the reported "document tabs don't scroll, the wheel
 * does nothing". It is intermittent because it depends on whether that first
 * resize is observed, which is why the same file opens correctly on one attempt
 * and blank on the next.
 *
 * So the size is taken from the host box rather than waited for: one layout when
 * the editor mounts, and one on every later change of that box. `attach` returns
 * the teardown, and is written against the two shapes `@monaco-editor/react`
 * mounts — a plain editor and a diff editor — because both had the same defect.
 */

import { useCallback, useEffect, useRef } from "react";

/** The part of Monaco's editor API this needs, so a test can supply it. */
export interface LayoutableEditor {
  layout(dimension?: { width: number; height: number }): void;
  getContainerDomNode?(): HTMLElement | null;
  getDomNode?(): HTMLElement | null;
}

/**
 * Lays the editor out at its host's size now, and again whenever that changes.
 *
 * The host is the element Monaco was mounted into, walked up one level: that is
 * the box the app sized, while Monaco's own node is the thing being sized to it
 * and would measure itself in a circle.
 */
export function attachEditorLayout(
  editor: LayoutableEditor,
  observe: (target: Element, callback: () => void) => () => void = observeResize,
): () => void {
  const node = editor.getContainerDomNode?.() ?? editor.getDomNode?.() ?? undefined;
  const host = node?.parentElement ?? node;
  // No host to measure: a single layout is still better than none, because the
  // container is visible by the time this runs and Monaco's own measurement of
  // it is what was missed.
  if (!host) {
    editor.layout();
    return () => undefined;
  }
  const apply = () => {
    const box = host.getBoundingClientRect();
    // A zero box is the pane being hidden, not a resize to nothing. Laying out
    // at zero there would throw away the size and reproduce the defect on the
    // way back.
    if (box.width > 0 && box.height > 0) editor.layout({ width: box.width, height: box.height });
  };
  apply();
  return observe(host, apply);
}

/**
 * Attaches the layout observer for as long as the editor is mounted.
 *
 * This is how the app uses `attachEditorLayout`; that function stays exported
 * as the DOM-level primitive the tests drive directly, because everything
 * interesting about it — the zero box, the missing host — is about the box and
 * not about React.
 *
 * The lifetime is the editor component's, not the tab's. Both surfaces
 * previously detached only when the whole tab lifecycle ended, so an editor
 * that came and went inside one tab — a Markdown view switched to preview and
 * back, a diff replaced by a binary one — left its `ResizeObserver` attached to
 * a box whose editor no longer existed, once per switch.
 *
 * The returned callback is idempotent by replacement: calling it again is
 * "this component's editor is now that one", which is what an editor swapped
 * in place would need, and it detaches the previous observer rather than
 * leaking it.
 */
export function useEditorLayout(): (editor: LayoutableEditor) => void {
  const detach = useRef<(() => void) | undefined>(undefined);
  useEffect(() => () => {
    detach.current?.();
    detach.current = undefined;
  }, []);
  return useCallback((editor: LayoutableEditor) => {
    detach.current?.();
    detach.current = attachEditorLayout(editor);
  }, []);
}

function observeResize(target: Element, callback: () => void): () => void {
  if (typeof ResizeObserver !== "function") return () => undefined;
  const observer = new ResizeObserver(() => callback());
  observer.observe(target);
  return () => observer.disconnect();
}
