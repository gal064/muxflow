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
 * Between a tab strip and a panel that read the token file, that is a pasted-in
 * second application, and it is exactly what 11.1.1's "one token file" exists
 * to prevent.
 *
 * The syntax colours are the terminal's ANSI palette. Phase 11 left them at
 * `vs-dark`'s, on the argument that the token table said nothing about them —
 * true then, when the editor ground was a near-black the vs-dark colours were
 * tuned for. The ground is `--chrome-bg` and `--chrome-bg` is now the terminal
 * background, which vs-dark's darker syntax colours sit on at a contrast they
 * were never chosen for. The mapping below is deliberately small: Monaco's token
 * names are coarse, and a full TextMate table would be a much larger claim than
 * "an editor and a terminal showing the same file should agree about what a
 * string looks like".
 */
export const ADE_MONACO_THEME = "ade-dark";

export function defineAdeMonacoTheme(root: Element | undefined = globalThis.document?.documentElement): void {
  const read = tokenReader(root);
  const token = (name: string, fallback: string) => read(name) ?? fallback;
  monaco.editor.defineTheme(ADE_MONACO_THEME, {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: syntaxColor(token("--term-8", "#666666")), fontStyle: "italic" },
      { token: "keyword", foreground: syntaxColor(token("--term-5", "#b294bb")) },
      { token: "string", foreground: syntaxColor(token("--term-2", "#b6bd68")) },
      { token: "number", foreground: syntaxColor(token("--term-3", "#f0c674")) },
      { token: "type", foreground: syntaxColor(token("--term-6", "#8abeb7")) },
      { token: "function", foreground: syntaxColor(token("--term-4", "#82a2be")) },
      { token: "variable", foreground: syntaxColor(token("--term-7", "#c4c8c6")) },
    ],
    colors: {
      "editor.background": token("--chrome-bg", "#282c34"),
      "editor.foreground": token("--chrome-ink", "#c4c8c6"),
      "editorGutter.background": token("--chrome-bg", "#282c34"),
      "editorLineNumber.foreground": token("--chrome-faint", "#565e6a"),
      "editorLineNumber.activeForeground": token("--chrome-ink", "#c4c8c6"),
      "editor.lineHighlightBackground": token("--chrome-hover", "#2f343e"),
      "editor.selectionBackground": token("--accent-wash", "#7aa6da1f"),
      "editorCursor.foreground": token("--accent", "#7aa6da"),
      "editorWidget.background": token("--chrome-raised", "#2c313a"),
      "editorWidget.border": token("--chrome-border", "#3e4451"),
      "editorIndentGuide.background1": token("--chrome-hairline", "#313640"),
      "editorOverviewRuler.border": token("--chrome-hairline", "#313640"),
      "scrollbarSlider.background": token("--chrome-border", "#3e4451"),
      "scrollbarSlider.hoverBackground": token("--chrome-hover", "#2f343e"),
      "scrollbarSlider.activeBackground": token("--chrome-dim", "#8a919c"),
      "diffEditor.insertedTextBackground": "#2ea04326",
      "diffEditor.removedTextBackground": "#cc656626",
    },
  });
}

/**
 * A syntax rule's colour, or nothing if the token cannot be one.
 *
 * The `colors` map above accepts any CSS colour; a `rules` entry does not.
 * Monaco matches a rule's `foreground` against `/^#?[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$/`
 * and **throws** on anything else — from `defineTheme`, which this module calls
 * at import time, so one unparseable token would take the whole editor bundle
 * down rather than mis-colour a keyword. Omitting the foreground instead leaves
 * that rule inheriting `vs-dark`'s, which is the state this table replaced and a
 * safe place to land.
 */
function syntaxColor(value: string): string | undefined {
  return /^#?[0-9a-fA-F]{6}$/.test(value) ? value : undefined;
}

defineAdeMonacoTheme();
