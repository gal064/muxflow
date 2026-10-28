import { loader } from "@monaco-editor/react";
import { terminalFacesReady, terminalTheme, tokenWithFallback } from "../terminal/theme";
import { currentTheme, onAppearanceChange } from "../../ui/appearance";
import * as monaco from "monaco-editor";
import CssWorker from "monaco-editor/language/css/css.worker?worker";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";
import HtmlWorker from "monaco-editor/language/html/html.worker?worker";
import JsonWorker from "monaco-editor/language/json/json.worker?worker";
import TypeScriptWorker from "monaco-editor/language/typescript/ts.worker?worker";
import { extendPython, extendTypeScript } from "./monarch";

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

// Replaces the bundled grammar factories with the extended ones. A factory, not
// a provider, so each grammar still loads the first time a file in that
// language opens, from the same chunk the bundled factory would have used.
monaco.languages.registerTokensProviderFactory("python", {
  create: () => import("monaco-editor/languages/definitions/python/python").then((m) => extendPython(m.language)),
});
monaco.languages.registerTokensProviderFactory("typescript", {
  create: () => import("monaco-editor/languages/definitions/typescript/typescript").then((m) => extendTypeScript(m.language)),
});
monaco.languages.registerTokensProviderFactory("javascript", {
  create: () => import("monaco-editor/languages/definitions/javascript/javascript").then((m) => extendTypeScript(m.language)),
});

// Monaco measures glyph widths once, when the first editor mounts. If that is
// before JetBrains Mono has loaded it measures the fallback face, and every
// cursor and selection is then off by the difference.
void terminalFacesReady().then(() => monaco.editor.remeasureFonts());

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
 * The syntax colours are the terminal's ANSI palette, and plain code is the
 * terminal's own foreground, so an editor and a terminal showing the same file
 * agree about what text and a string look like. `vs-dark`'s colours were tuned
 * for a near-black ground; this ground is the terminal background, and the
 * chrome ink it used to draw code in left most of a file at a contrast well
 * below what the terminal beside it shows.
 *
 * The rows follow what the grammars emit. Python and TypeScript go through
 * `monarch.ts`, which adds `function`, `predefined`, `constant`,
 * `variable.language`, `annotation` and Python's `operator`; the other bundled
 * grammars emit a subset of the same names. Two rows are deliberate choices:
 *
 *  - Punctuation and operators take `--chrome-dim`, so the names between them
 *    carry the line.
 *  - `comment` is the one row that is deliberately *not* an ANSI colour.
 *    `--term-8` is Ghostty's dim grey — dim against a terminal's brighter
 *    neighbours, and only 2.44:1 on this ground, which is worse than the
 *    `vs-dark` green it would replace (3.53:1) on the highest-volume quiet token
 *    in an editor. It takes `--chrome-dim`, the app's own quiet ink, picked for
 *    legibility on exactly this background (4.70:1) by exactly this argument.
 */
export const ADE_MONACO_THEME = "ade";

export function defineAdeMonacoTheme(root: Element | undefined = globalThis.document?.documentElement): void {
  // Neither map below holds a colour of its own. The chrome half reads through
  // the fallback reader, whose values `theme.test.ts` pins to `tokens.css`; the
  // syntax half reads the resolved terminal palette, which the same test pins.
  // The only literals left are the two diff washes at the bottom, which are
  // blend colours rather than theme colours and are called out there.
  //
  // One asymmetry worth knowing before editing a token these read: `colors`
  // takes any CSS colour, `rules` does not. A rule foreground must be six or
  // eight hex digits — Monaco *throws* on anything else, at first editor mount
  // rather than here — so a `--term-*` or `--chrome-dim` written as `rgb(…)` or
  // as a `var()` reference would take the editor surface down rather than
  // degrade. `theme.test.ts` pins every one of them to exact hex today, which
  // is what makes that safe.
  const token = tokenWithFallback(root);
  const ansi = terminalTheme(root);
  monaco.editor.defineTheme(ADE_MONACO_THEME, {
    base: currentTheme(root) === "light" ? "vs" : "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: token("--chrome-dim"), fontStyle: "italic" },
      { token: "keyword", foreground: ansi.brightMagenta },
      { token: "string", foreground: ansi.green },
      { token: "number", foreground: ansi.yellow },
      { token: "constant", foreground: ansi.yellow },
      { token: "annotation", foreground: ansi.yellow },
      { token: "type", foreground: ansi.cyan },
      { token: "predefined", foreground: ansi.cyan },
      { token: "function", foreground: ansi.brightBlue },
      { token: "variable.language", foreground: ansi.red, fontStyle: "italic" },
      { token: "regexp", foreground: ansi.red },
      { token: "regexp.escape", foreground: ansi.yellow },
      { token: "delimiter", foreground: token("--chrome-dim") },
      { token: "operator", foreground: token("--chrome-dim") },
    ],
    colors: {
      "editor.background": token("--chrome-bg"),
      // `terminalTheme` always fills the foreground; the type just cannot say so.
      "editor.foreground": ansi.foreground ?? token("--chrome-ink"),
      "editorGutter.background": token("--chrome-bg"),
      "editorLineNumber.foreground": token("--chrome-faint"),
      "editorLineNumber.activeForeground": token("--chrome-ink"),
      "editor.lineHighlightBackground": token("--chrome-hover"),
      "editor.selectionBackground": token("--accent-wash"),
      "editorCursor.foreground": token("--accent"),
      "editorWidget.background": token("--chrome-raised"),
      "editorWidget.border": token("--chrome-border"),
      "editorIndentGuide.background1": token("--chrome-border"),
      "editorIndentGuide.activeBackground1": token("--chrome-faint"),
      // Bracket-pair colouring is on by default and otherwise cycles Monaco's
      // own gold, orchid and azure, the loudest colours on any line.
      "editorBracketHighlight.foreground1": ansi.yellow ?? token("--chrome-ink"),
      "editorBracketHighlight.foreground2": ansi.magenta ?? token("--chrome-ink"),
      "editorBracketHighlight.foreground3": ansi.blue ?? token("--chrome-ink"),
      "editorOverviewRuler.border": token("--chrome-hairline"),
      "scrollbarSlider.background": token("--chrome-border"),
      "scrollbarSlider.hoverBackground": token("--chrome-hover"),
      "scrollbarSlider.activeBackground": token("--chrome-dim"),
      // Translucent additions over whatever line the diff lands on. The
      // added-line green has no counterpart in the palette and reads on either
      // theme; the removed-line red is `--danger` at an alpha `--danger-wash`
      // does not carry. A `colors` entry takes any CSS colour, unlike a `rules`
      // entry, which is why these can live here.
      "diffEditor.insertedTextBackground": "#2ea04326",
      "diffEditor.removedTextBackground": `${token("--danger")}26`,
    },
  });
}

defineAdeMonacoTheme();
// Monaco themes are global: redefining the one theme and setting it again
// repaints every open editor and diff.
onAppearanceChange(() => {
  defineAdeMonacoTheme();
  monaco.editor.setTheme(ADE_MONACO_THEME);
});
