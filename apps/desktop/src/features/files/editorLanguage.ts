/**
 * Monaco language id for a path, by extension.
 *
 * Deliberately a table rather than Monaco's own registry lookup: the mapping is
 * the exact set of languages this app claims to highlight, it is answerable
 * without the editor chunk having been evaluated, and it is the one place to
 * read when deciding what the bundled language surface is actually used for.
 *
 * Exported for that last reason. It is the measured half of the plan's
 * "measure Monaco capability use", and `editorLanguage.test.ts` pins it so that
 * narrowing the bundled surface has to be a decision with a diff rather than a
 * silent loss of highlighting.
 */
export const LANGUAGE_BY_EXTENSION: Readonly<Record<string, string>> = Object.freeze({
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  rs: "rust",
  py: "python",
  md: "markdown",
  json: "json",
  css: "css",
  html: "html",
  sh: "shell",
  yml: "yaml",
  yaml: "yaml",
  toml: "ini",
});

export function languageForPath(path: string): string {
  const extension = path.split(".").at(-1)?.toLowerCase();
  return LANGUAGE_BY_EXTENSION[extension ?? ""] ?? "plaintext";
}
