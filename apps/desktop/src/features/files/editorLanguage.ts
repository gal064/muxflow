/**
 * Monaco language id for a path, by extension.
 *
 * Deliberately a table rather than Monaco's own registry lookup: the mapping is
 * the exact set of languages this app claims to highlight, it is answerable
 * without the editor chunk having been evaluated, and it is the one place to
 * read when deciding what the bundled language surface is actually used for.
 */
const LANGUAGE_BY_EXTENSION: Readonly<Record<string, string>> = {
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
};

export function languageForPath(path: string): string {
  const extension = path.split(".").at(-1)?.toLowerCase();
  return LANGUAGE_BY_EXTENSION[extension ?? ""] ?? "plaintext";
}

/**
 * Every language id the app can ask an editor for.
 *
 * The measured half of the plan's "measure Monaco capability use": the bundle
 * ships every language it knows, this is the set anything ever requests, and
 * `editorLanguage.test.ts` pins it so that narrowing the bundled surface has to
 * be a decision with a diff rather than a silent loss of highlighting. Its only
 * caller is that test, deliberately — it exists to be pinned.
 */
export function requestedLanguageIds(): readonly string[] {
  return [...new Set(Object.values(LANGUAGE_BY_EXTENSION))].sort();
}
