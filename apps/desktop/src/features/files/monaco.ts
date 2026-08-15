import { loader } from "@monaco-editor/react";
import { terminalTheme, tokenWithFallback } from "../terminal/theme";
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
 *
 * Small also means each row has to earn its place, so two were checked against
 * the bundled Monarch grammars rather than assumed:
 *
 *  - `function` and `variable` are absent, because nothing emits them where it
 *    would show. TypeScript and Rust emit `identifier`, never `function`, and
 *    `variable` appears only in Markdown's link definitions. A rule that colours
 *    nothing is a rule nobody notices going wrong.
 *  - `comment` is the one row that is deliberately *not* an ANSI colour.
 *    `--term-8` is Ghostty's dim grey — dim against a terminal's brighter
 *    neighbours, and only 2.44:1 on this ground, which is worse than the
 *    `vs-dark` green it would replace (3.53:1) on the highest-volume quiet token
 *    in an editor. It takes `--chrome-dim`, the app's own quiet ink, picked for
 *    legibility on exactly this background (4.70:1) by exactly this argument.
 */
export const ADE_MONACO_THEME = "ade-dark";

export function defineAdeMonacoTheme(root: Element | undefined = globalThis.document?.documentElement): void {
  // Neither map below holds a colour of its own. The chrome half reads through
  // the fallback reader, whose values `theme.test.ts` pins to `tokens.css`; the
  // syntax half reads the resolved terminal palette, which the same test pins.
  // The only literals left are the two diff washes at the bottom, which are
  // blend colours rather than theme colours and are called out there.
  const token = tokenWithFallback(root);
  const ansi = terminalTheme(root);
  monaco.editor.defineTheme(ADE_MONACO_THEME, {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: token("--chrome-dim"), fontStyle: "italic" },
      { token: "keyword", foreground: ansi.magenta },
      { token: "string", foreground: ansi.green },
      { token: "number", foreground: ansi.yellow },
      { token: "type", foreground: ansi.cyan },
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
      // Translucent additions over whatever line the diff lands on, so they are
      // eight digits and not a token: the added-line green has no counterpart in
      // the palette at all, and the removed-line red is `--danger` at an alpha
      // `--danger-wash` does not carry. A `colors` entry takes any CSS colour,
      // unlike a `rules` entry, which is why these can live here.
      "diffEditor.insertedTextBackground": "#2ea04326",
      "diffEditor.removedTextBackground": "#cc656626",
    },
  });
}

defineAdeMonacoTheme();
