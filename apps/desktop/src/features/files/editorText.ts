import type { editor } from "monaco-editor";
import { terminalFont } from "../terminal/theme";

/**
 * How every Monaco surface sets its text: the terminal's face and size.
 *
 * Unset, Monaco picks its own platform default (Menlo 12px on macOS, a Droid
 * Sans Mono fallback on Linux), so a file opened beside a terminal showed the
 * same code smaller and in a different face. `1.5` is a multiple of the font
 * size, which Monaco rounds to whole pixels: 20px lines at 13px.
 *
 * Kept apart from `monaco.ts` so the editors can read it without that module,
 * which the editor-chunk tests replace wholesale.
 */
export function editorTextOptions(root?: Element): editor.IEditorOptions {
  const { fontFamily, fontSize } = terminalFont(root);
  return { fontFamily, fontSize, lineHeight: 1.5, fontLigatures: false };
}
