// What the terminal's Send types: the reply field's text, then the paths of
// its files, as the desktop pastes an upload's (terminal/integration/upload.rs).

import { isImage } from "../chat/attach/attachState";

/** A path an agent takes as an image attachment: absolute, with nothing to quote. */
const BARE_IMAGE_PATH = /^\/[^\s'"\\\x7f]*$/;

/**
 * One file's path as it is typed. An image goes bare, as agents recognize an
 * attachment only by its bare path; anything else is single-quoted for the
 * shell, and a relative name that starts with a dash is made explicitly
 * relative so a command cannot read it as an option.
 */
export function typedPath(file: { path: string; name: string; mediaType: string }): string {
  if (isImage(file) && BARE_IMAGE_PATH.test(file.path)) return file.path;
  const safe = file.path.startsWith("-") && !file.path.includes("/") ? `./${file.path}` : file.path;
  return `'${safe.replace(/'/g, `'"'"'`)}'`;
}

/** The text as typed, then each file's path, one space apart. */
export function withFilePaths(text: string, files: readonly { path: string; name: string; mediaType: string }[]): string {
  if (files.length === 0) return text;
  return [text.trimEnd(), ...files.map(typedPath)].filter((part) => part !== "").join(" ");
}
