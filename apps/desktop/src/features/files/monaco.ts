import { loader } from "@monaco-editor/react";
import { tokenReader } from "../terminal/theme";
import * as monaco from "monaco-editor";
import CssWorker from "monaco-editor/language/css/css.worker?worker";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import HtmlWorker from "monaco-editor/language/html/html.worker?worker";
import JsonWorker from "monaco-editor/language/json/json.worker?worker";
import TypeScriptWorker from "monaco-editor/language/typescript/ts.worker?worker";

self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string): Worker {
    if (label === "json") return new JsonWorker();
    if (["css", "scss", "less"].includes(label)) return new CssWorker();
    if (["html", "handlebars", "razor"].includes(label)) return new HtmlWorker();
    if (["typescript", "javascript"].includes(label)) return new TypeScriptWorker();
    return new EditorWorker();
  },
};

// @monaco-editor/react otherwise defaults to a CDN loader. The desktop must
// work offline and remote-host selection must never influence editor assets.
loader.config({ monaco });

/**
 * The document surfaces read from the same token file as everything else.
 *
 * 11.3 makes editors, diffs and Markdown first-class peers of the terminal, in
 * the same tab strip — and they were the one surface still painted by a foreign
 * palette, Monaco's built-in `vs-dark` at `#1E1E1E` with a `#3A3B3C` scrollbar.
 * Between a `#101114` tab strip and a `#101114` panel that reads as a pasted-in
 * second application, and it is exactly what 11.1.1's "one token file" exists
 * to prevent.
 *
 * Only the *chrome* of the editor is overridden. Syntax colors stay `vs-dark`'s:
 * the token table says nothing about them, and inventing a language palette
 * here would be a bigger claim than this phase is making.
 */
export const ADE_MONACO_THEME = "ade-dark";

export function defineAdeMonacoTheme(root: Element | undefined = globalThis.document?.documentElement): void {
  const read = tokenReader(root);
  const token = (name: string, fallback: string) => read(name) ?? fallback;
  monaco.editor.defineTheme(ADE_MONACO_THEME, {
    base: "vs-dark",
    inherit: true,
    rules: [],
    colors: {
      "editor.background": token("--chrome-bg", "#101114"),
      "editor.foreground": token("--chrome-ink", "#c9cdd3"),
      "editorGutter.background": token("--chrome-bg", "#101114"),
      "editorLineNumber.foreground": token("--chrome-faint", "#4d545e"),
      "editorLineNumber.activeForeground": token("--chrome-ink", "#c9cdd3"),
      "editor.lineHighlightBackground": token("--chrome-hover", "#1a1d22"),
      "editor.selectionBackground": token("--accent-wash", "#0091ff1f"),
      "editorCursor.foreground": token("--accent", "#0091ff"),
      "editorWidget.background": token("--chrome-raised", "#16181c"),
      "editorWidget.border": token("--chrome-border", "#26292f"),
      "editorIndentGuide.background1": token("--chrome-hairline", "#1c1e22"),
      "editorOverviewRuler.border": token("--chrome-hairline", "#1c1e22"),
      "scrollbarSlider.background": token("--chrome-border", "#26292f"),
      "scrollbarSlider.hoverBackground": token("--chrome-hover", "#1a1d22"),
      "scrollbarSlider.activeBackground": token("--chrome-dim", "#7d848e"),
      "diffEditor.insertedTextBackground": "#2ea04326",
      "diffEditor.removedTextBackground": "#cc656626",
    },
  });
}

defineAdeMonacoTheme();
