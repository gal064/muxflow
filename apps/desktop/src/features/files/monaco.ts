import { loader } from "@monaco-editor/react";
import { CHROME_FALLBACKS, terminalTheme, tokenReader } from "../terminal/theme";
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
  // No literals here, and no second list of them: the fallbacks are the ones
  // `theme.test.ts` already pins to `tokens.css`, so this file cannot be the
  // place the palette drifts.
  const token = (name: keyof typeof CHROME_FALLBACKS) => read(name) ?? CHROME_FALLBACKS[name];
  const ansi = terminalTheme(root);
  monaco.editor.defineTheme(ADE_MONACO_THEME, {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: syntaxColor(ansi.brightBlack), fontStyle: "italic" },
      { token: "keyword", foreground: syntaxColor(ansi.magenta) },
      { token: "string", foreground: syntaxColor(ansi.green) },
      { token: "number", foreground: syntaxColor(ansi.yellow) },
      { token: "type", foreground: syntaxColor(ansi.cyan) },
      { token: "function", foreground: syntaxColor(ansi.blue) },
      { token: "variable", foreground: syntaxColor(ansi.white) },
    ],
    colors: {
      "editor.background": token("--chrome-bg"),
      "editor.foreground": token("--chrome-ink"),
      "editorGutter.background": token("--chrome-bg"),
      "editorLineNumber.foreground": token("--chrome-faint"),
      "editorLineNumber.activeForeground": token("--chrome-ink"),
      "editor.lineHighlightBackground": token("--chrome-hover"),
      "editor.selectionBackground": token("--accent-wash"),
      "editorCursor.foreground": token("--accent"),
      "editorWidget.background": token("--chrome-raised"),
      "editorWidget.border": token("--chrome-border"),
      "editorIndentGuide.background1": token("--chrome-hairline"),
      "editorOverviewRuler.border": token("--chrome-hairline"),
      "scrollbarSlider.background": token("--chrome-border"),
      "scrollbarSlider.hoverBackground": token("--chrome-hover"),
      "scrollbarSlider.activeBackground": token("--chrome-dim"),
      "diffEditor.insertedTextBackground": "#2ea04326",
      "diffEditor.removedTextBackground": "#cc656626",
    },
  });
}

/**
 * A syntax rule's colour, or nothing if the value cannot be one.
 *
 * The `colors` map above accepts any CSS colour; a `rules` entry does not.
 * Monaco matches a rule's `foreground` against
 * `/^#?[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$/` and **throws** on anything else —
 * from `defineTheme`, which this module calls at import time, so one
 * unparseable value would take the whole editor bundle down rather than
 * mis-colour a keyword. Omitting the foreground instead leaves that rule
 * inheriting `vs-dark`'s, which is the state this table replaced and a safe
 * place to land.
 *
 * The alpha form Monaco tolerates is refused here on purpose: it discards the
 * alpha pair anyway, so a translucent token would silently paint opaque.
 */
function syntaxColor(value: string | undefined): string | undefined {
  return value !== undefined && /^#?[0-9a-fA-F]{6}$/.test(value) ? value : undefined;
}

defineAdeMonacoTheme();
