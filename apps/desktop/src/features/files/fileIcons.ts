import type { IconName } from "../../ui/Icon";
import type { FileEntry } from "./types";

/**
 * One file-type mapping, read by the Explorer tree and the tab strip.
 *
 * VS Code's Seti icons are recognisable mostly by *color*: a handful of shape
 * archetypes tinted per language. This follows that, using the terminal's own
 * ANSI palette so the Explorer cannot drift from the theme, and keeps the
 * mapping in one place so a tab and its row never disagree about what a file
 * is.
 *
 * `color` is a CSS custom-property reference, applied as the icon's
 * `currentColor` by whichever surface renders it.
 */
export interface FileIconChoice {
  icon: IconName;
  color: string;
}

const DIRECTORY_COLOR = "var(--term-4)";
/** A symlink keeps its target's shape and takes the tint the `l` glyph had. */
const SYMLINK_COLOR = "var(--term-6)";
const FALLBACK: FileIconChoice = { icon: "file", color: "var(--chrome-dim)" };
const CONFIG: FileIconChoice = { icon: "fileConfig", color: "var(--chrome-dim)" };

const EXTENSION_ICONS: Record<string, FileIconChoice> = {
  ts: { icon: "fileCode", color: "var(--term-4)" },
  tsx: { icon: "fileCode", color: "var(--term-4)" },
  mts: { icon: "fileCode", color: "var(--term-4)" },
  cts: { icon: "fileCode", color: "var(--term-4)" },
  js: { icon: "fileCode", color: "var(--term-3)" },
  jsx: { icon: "fileCode", color: "var(--term-3)" },
  mjs: { icon: "fileCode", color: "var(--term-3)" },
  cjs: { icon: "fileCode", color: "var(--term-3)" },
  rs: { icon: "fileCode", color: "var(--term-1)" },
  py: { icon: "fileCode", color: "var(--term-2)" },
  json: { icon: "fileData", color: "var(--term-3)" },
  jsonc: { icon: "fileData", color: "var(--term-3)" },
  toml: { icon: "fileData", color: "var(--term-3)" },
  yml: { icon: "fileData", color: "var(--term-3)" },
  yaml: { icon: "fileData", color: "var(--term-3)" },
  md: { icon: "markdown", color: "var(--term-4)" },
  mdown: { icon: "markdown", color: "var(--term-4)" },
  markdown: { icon: "markdown", color: "var(--term-4)" },
  css: { icon: "fileCode", color: "var(--term-5)" },
  scss: { icon: "fileCode", color: "var(--term-5)" },
  sass: { icon: "fileCode", color: "var(--term-5)" },
  less: { icon: "fileCode", color: "var(--term-5)" },
  html: { icon: "fileCode", color: "var(--term-1)" },
  htm: { icon: "fileCode", color: "var(--term-1)" },
  sh: { icon: "fileShell", color: "var(--term-2)" },
  zsh: { icon: "fileShell", color: "var(--term-2)" },
  bash: { icon: "fileShell", color: "var(--term-2)" },
  fish: { icon: "fileShell", color: "var(--term-2)" },
  png: { icon: "fileImage", color: "var(--term-5)" },
  jpg: { icon: "fileImage", color: "var(--term-5)" },
  jpeg: { icon: "fileImage", color: "var(--term-5)" },
  gif: { icon: "fileImage", color: "var(--term-5)" },
  webp: { icon: "fileImage", color: "var(--term-5)" },
  avif: { icon: "fileImage", color: "var(--term-5)" },
  bmp: { icon: "fileImage", color: "var(--term-5)" },
  ico: { icon: "fileImage", color: "var(--term-5)" },
  svg: { icon: "fileImage", color: "var(--term-5)" },
  lock: { icon: "fileLock", color: "var(--chrome-faint)" },
  txt: { icon: "fileText", color: "var(--chrome-dim)" },
  log: { icon: "fileText", color: "var(--chrome-dim)" },
};

export function fileIcon(
  entry: { name: string; kind: FileEntry["kind"]; targetKind?: FileEntry["targetKind"] },
  expanded = false,
): FileIconChoice {
  const directory = entry.kind === "directory" || (entry.kind === "symlink" && entry.targetKind === "directory");
  if (directory) {
    return { icon: expanded ? "folderOpen" : "folder", color: entry.kind === "symlink" ? SYMLINK_COLOR : DIRECTORY_COLOR };
  }
  const shape = shapeForName(entry.name);
  return entry.kind === "symlink" ? { icon: shape.icon, color: SYMLINK_COLOR } : shape;
}

function shapeForName(name: string): FileIconChoice {
  const lower = name.toLowerCase();
  // A dotfile is configuration first and a language second: `.eslintrc.js` is
  // read as "the eslint config", not "some JavaScript". `*.config.*` is the
  // same claim spelled the other way round, and VS Code marks both.
  if (lower.startsWith(".") || /\.config\.[^.]+$/u.test(lower)) return CONFIG;
  const dot = lower.lastIndexOf(".");
  if (dot < 0) return FALLBACK;
  return EXTENSION_ICONS[lower.slice(dot + 1)] ?? FALLBACK;
}
