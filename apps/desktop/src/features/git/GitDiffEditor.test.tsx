// @vitest-environment jsdom
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { editor } from "monaco-editor";
import { GitDiffEditor } from "./GitDiffEditor";

let lastOptions: editor.IDiffEditorConstructionOptions | undefined;
vi.mock("@monaco-editor/react", () => ({
  DiffEditor: (props: { options: editor.IDiffEditorConstructionOptions }) => {
    lastOptions = props.options;
    return <div>diff editor</div>;
  },
}));
vi.mock("../files/monaco", () => ({ ADE_MONACO_THEME: "ade-dark" }));

describe("GitDiffEditor", () => {
  it("wraps long lines on both sides of the diff", () => {
    renderToStaticMarkup(<GitDiffEditor
      modified={"b\n"} modifiedModelPath="git://modified/a.ts" onReady={() => {}}
      original={"a\n"} originalModelPath="git://original/a.ts" path="a.ts"
    />);
    expect(lastOptions?.wordWrap).toBe("on");
    // Without `inherit` the original side keeps its own (off) wrapping and the
    // two sides stop lining up, which is worse than not wrapping at all.
    expect(lastOptions?.diffWordWrap).toBe("inherit");
    // Wrapping must not cost the side-by-side layout or its resizer.
    expect(lastOptions?.renderSideBySide).toBe(true);
    expect(lastOptions?.enableSplitViewResizing).toBe(true);
  });
});
