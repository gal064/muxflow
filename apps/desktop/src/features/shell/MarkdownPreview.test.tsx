// @vitest-environment jsdom
// jsdom, because the preview reads the document's selection and listens for the
// end of a pointer gesture on the document itself.
import { act as domAct } from "react";
import { createRoot } from "react-dom/client";
import { act, create } from "react-test-renderer";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MarkdownPreview } from "./AppTabSurface";
import { useSanitizedMarkdown } from "../files/markdownPreview";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** Stands in for a text node the selection is anchored at inside the article. */
const insideArticle = {};
/**
 * The element behind every host ref in this tree.
 *
 * `contains` is the one the preview asks about; the rest keep the confirmation
 * dialog's focus trap from tripping over a bare object.
 */
const articleNode = {
  contains: (node: unknown) => node === insideArticle,
  closest: () => null,
  querySelector: () => null,
  focus: () => undefined,
  isConnected: false,
  tabIndex: 0,
};

let selection: { isCollapsed: boolean; anchorNode: unknown; focusNode?: unknown } | null = null;
const realGetSelection = document.getSelection;

beforeEach(() => {
  selection = null;
  document.getSelection = (() => selection) as typeof document.getSelection;
});

afterEach(() => {
  document.getSelection = realGetSelection;
  vi.useRealTimers();
});

function select(anchorNode: unknown) { selection = { isCollapsed: false, anchorNode, focusNode: anchorNode }; }

function preview(source: string, onStatus = vi.fn()) {
  let renderer!: ReturnType<typeof create>;
  act(() => {
    renderer = create(<MarkdownPreview source={source} onStatus={onStatus} />, { createNodeMock: () => articleNode });
  });
  const article = () => renderer.root.findByProps({ className: "markdown-preview" });
  return {
    renderer,
    onStatus,
    article,
    html: () => article().props.dangerouslySetInnerHTML.__html as string,
    retype: async (next: string) => {
      await act(async () => { renderer.update(<MarkdownPreview source={next} onStatus={onStatus} />); });
    },
    /** Long enough for the debounce and the idle fallback behind it. */
    settle: async () => { await act(async () => { await vi.advanceTimersByTimeAsync(500); }); },
    fire: async (type: string) => { await act(async () => { document.dispatchEvent(new Event(type)); }); },
  };
}

function press(surface: ReturnType<typeof preview>, x = 10, y = 10) {
  act(() => { surface.article().props.onPointerDown({ button: 0, clientX: x, clientY: y }); });
}

/** `detail` 0 is a keyboard activation, which carries no pointer coordinates. */
function click(surface: ReturnType<typeof preview>, href: string | null, x = 10, y = 10, detail = 1) {
  const preventDefault = vi.fn();
  const anchor = href === null ? null : { getAttribute: (name: string) => (name === "href" ? href : null) };
  act(() => {
    surface.article().props.onClick({
      clientX: x, clientY: y, detail, preventDefault,
      target: { closest: (selector: string) => (selector === "a" ? anchor : null) },
    });
  });
  return preventDefault;
}

const opened = (surface: ReturnType<typeof preview>) => JSON.stringify(surface.renderer.toJSON()).includes("Open external link?");

describe("MarkdownPreview selection", () => {
  it("does not replace the rendered text while a pointer gesture is in progress", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    press(surface);
    await surface.retype("after");
    await surface.settle();
    expect(surface.html(), "an autosave-driven refresh landed in the middle of a drag").toContain("before");

    await surface.fire("pointerup");
    expect(surface.html(), "the update was dropped rather than deferred").toContain("after");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("also releases the held update when the gesture is cancelled", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    press(surface);
    await surface.retype("after");
    await surface.settle();
    await surface.fire("pointercancel");
    expect(surface.html()).toContain("after");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("holds a refresh while text inside the preview is selected, and publishes once it is gone", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    select(insideArticle);
    await surface.retype("after");
    await surface.settle();
    expect(surface.html(), "a pending refresh moved a live selection").toContain("before");

    // Selection collapsed by a click elsewhere: nothing is at risk any more.
    selection = null;
    await surface.fire("selectionchange");
    expect(surface.html()).toContain("after");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("drops a held update when the source goes back to what is on screen", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    select(insideArticle);
    await surface.retype("after");
    await surface.settle();
    expect(surface.html()).toContain("before");

    // An undo, or an agent restoring a file it had rewritten. What is parked
    // now describes a document that no longer exists.
    await surface.retype("before");
    await surface.settle();
    selection = null;
    await surface.fire("selectionchange");
    expect(surface.html(), "a superseded park was published over the live buffer").not.toContain("after");
    expect(surface.html()).toContain("before");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("holds a refresh for a selection that only reaches into the preview", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    // Dragged from the editor pane in split mode: anchored outside, ending in.
    selection = { isCollapsed: false, anchorNode: {} };
    (selection as { focusNode?: unknown }).focusNode = insideArticle;
    await surface.retype("after");
    await surface.settle();
    expect(surface.html()).toContain("before");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("ignores a selection that is not in the preview", async () => {
    vi.useFakeTimers();
    const surface = preview("before");
    select({});
    await surface.retype("after");
    await surface.settle();
    expect(surface.html(), "a selection in another surface froze this one").toContain("after");
    await act(async () => { surface.renderer.unmount(); });
  });

  it("opens a link on a plain click and leaves a drag alone", () => {
    const surface = preview("[site](https://example.com)");
    press(surface, 10, 10);
    expect(click(surface, "https://example.com", 11, 10), "a plain click stopped acting as a link").toHaveBeenCalled();
    expect(opened(surface)).toBe(true);
    act(() => { surface.renderer.unmount(); });
  });

  it("does not follow a link when the release ends a drag", () => {
    const surface = preview("[site](https://example.com)");
    press(surface, 10, 10);
    const preventDefault = click(surface, "https://example.com", 60, 10);
    expect(opened(surface), "a drag ending on a link was swallowed as a click").toBe(false);
    // Suppressed all the same: a click's default action is activation, and a
    // relative href left to the browser navigates the webview off the app.
    expect(preventDefault, "the browser was left free to follow the link").toHaveBeenCalled();
    act(() => { surface.renderer.unmount(); });
  });

  it("does not follow a link while a selection is standing", () => {
    const surface = preview("[site](https://example.com)");
    select(insideArticle);
    const preventDefault = click(surface, "https://example.com");
    expect(opened(surface)).toBe(false);
    expect(preventDefault, "the browser was left free to follow the link").toHaveBeenCalled();
    act(() => { surface.renderer.unmount(); });
  });

  it("activates a link from the keyboard after a drag that never produced a click", () => {
    const surface = preview("[site](https://example.com)");
    // Pressed in the preview, released outside it: no click reaches the
    // article, so the recorded press outlives its gesture.
    press(surface, 300, 300);
    expect(click(surface, "https://example.com", 0, 0, 0)).toHaveBeenCalled();
    expect(opened(surface), "a stale press swallowed a keyboard activation").toBe(true);
    act(() => { surface.renderer.unmount(); });
  });

  it("still reports a non-external link as a status message", () => {
    const surface = preview("[doc](./other.md)");
    press(surface);
    expect(click(surface, "./other.md")).toHaveBeenCalled();
    expect(surface.onStatus).toHaveBeenCalledWith("Markdown link: ./other.md");
    expect(opened(surface)).toBe(false);
    act(() => { surface.renderer.unmount(); });
  });
});

describe("useSanitizedMarkdown", () => {
  let renders = 0;
  function Harness({ source }: { source: string }) {
    renders += 1;
    return <span>{useSanitizedMarkdown(source)}</span>;
  }

  it("does not re-publish when the source changed but the rendered HTML did not", async () => {
    vi.useFakeTimers();
    renders = 0;
    let renderer!: ReturnType<typeof create>;
    act(() => { renderer = create(<Harness source="hello" />); });
    expect(renders).toBe(1);

    // A trailing newline is a different buffer and the same document.
    await act(async () => { renderer.update(<Harness source={"hello\n"} />); });
    expect(renders).toBe(2);
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(renders, "identical HTML was published back into the DOM").toBe(2);

    // A real edit still lands, so the skip is not just a stuck preview.
    await act(async () => { renderer.update(<Harness source="goodbye" />); });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(JSON.stringify(renderer.toJSON())).toContain("goodbye");
    await act(async () => { renderer.unmount(); });
  });
});

/**
 * The same behaviour against a real DOM, because the assertions above cannot
 * see it. `react-test-renderer` produces no nodes, so it reports the `__html`
 * prop and not what React did with it — and React 19 compares
 * `dangerouslySetInnerHTML` by reference, so a component that holds its
 * sanitize back can still rebuild the whole article on an unrelated re-render.
 * Only node identity distinguishes the two.
 */
describe("MarkdownPreview against the DOM", () => {
  let host: HTMLElement;
  let root: ReturnType<typeof createRoot>;

  beforeEach(() => {
    document.getSelection = realGetSelection;
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(async () => {
    await domAct(async () => { root.unmount(); });
    host.remove();
  });

  const render = async (source: string) => {
    await domAct(async () => { root.render(<MarkdownPreview source={source} onStatus={vi.fn()} />); });
  };
  const paragraph = () => host.querySelector("p")!;
  const send = (target: EventTarget, type: string, init: MouseEventInit = {}) => domAct(async () => {
    target.dispatchEvent(new MouseEvent(type, { bubbles: true, ...init }));
  });

  it("leaves the rendered nodes in place when a pointer gesture ends", async () => {
    await render("hello");
    const text = paragraph().firstChild;

    await send(host.querySelector("article")!, "pointerdown", { button: 0, clientX: 10, clientY: 10 });
    await send(document, "pointerup");
    // The component re-renders here to clear the gesture. Rebuilding the
    // article at that instant is precisely what drops the drag's selection.
    expect(paragraph().firstChild, "the gesture's own end rebuilt the text it selected").toBe(text);
  });

  it("keeps a live selection intact across an autosave refresh", async () => {
    vi.useFakeTimers();
    try {
      await render("hello");
      const selection = window.getSelection()!;
      const range = document.createRange();
      range.selectNodeContents(paragraph());
      selection.removeAllRanges();
      selection.addRange(range);
      expect(selection.toString()).toBe("hello");

      await render("hello there");
      await domAct(async () => { await vi.advanceTimersByTimeAsync(500); });
      expect(host.textContent?.trim(), "a pending refresh landed on a live selection").toBe("hello");
      expect(selection.toString(), "copy would have returned the wrong text").toBe("hello");

      // And once the selection is gone the deferred update arrives.
      selection.removeAllRanges();
      await domAct(async () => { document.dispatchEvent(new Event("selectionchange")); });
      expect(host.textContent?.trim()).toBe("hello there");
    } finally {
      vi.useRealTimers();
    }
  });

  it("holds a select-all, whose endpoints sit above the article", async () => {
    vi.useFakeTimers();
    try {
      await render("hello");
      const selection = window.getSelection()!;
      const range = document.createRange();
      range.selectNodeContents(document.body);
      selection.removeAllRanges();
      selection.addRange(range);

      await render("hello there");
      await domAct(async () => { await vi.advanceTimersByTimeAsync(500); });
      expect(host.textContent?.trim(), "select-all was not recognised as a selection to protect").toBe("hello");
    } finally {
      vi.useRealTimers();
    }
  });
});
