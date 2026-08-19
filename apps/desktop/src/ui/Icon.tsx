import type { ReactElement } from "react";

/**
 * The app's only icon set.
 *
 * Phase 11 deletes text-glyph icons (`•••`, `✎`, `▱`, `E`, `G`, `M`, `＋`):
 * they inherit the text font, so they changed shape per platform, sat on the
 * text baseline instead of the control's optical center, and could not be
 * given a consistent stroke weight. These are 16x16 stroke paths at
 * SF-Symbols weight, drawn in `currentColor`, so a control's color rule is the
 * only thing that decides how its icon looks.
 *
 * Icons are decorative by construction: every one renders `aria-hidden`, and
 * the control around it carries the accessible name.
 */

export type IconName =
  | "sidebarLeft"
  | "panelRight"
  | "bell"
  | "plus"
  // Source Control's row and header actions. `minus` is `plus` with one stroke
  // taken away, so stage and unstage read as one pair; `discard` is the return
  // arrow VS Code uses for it, and `refresh` the circular one.
  | "minus"
  | "discard"
  | "refresh"
  | "splitRight"
  | "close"
  | "chevronRight"
  | "chevronDown"
  | "file"
  | "markdown"
  | "diff"
  | "zoom"
  | "search"
  | "branch"
  // The Explorer's file-type set. VS Code's Seti look is mostly *color*, so
  // these are a handful of shape archetypes that a per-extension table tints
  // (`features/files/fileIcons.ts`) rather than one drawing per language.
  | "folder"
  | "folderOpen"
  | "fileCode"
  | "fileText"
  | "fileConfig"
  | "fileImage"
  | "fileLock"
  | "fileShell"
  | "fileData";

const paths: Record<IconName, ReactElement> = {
  sidebarLeft: <>
    <rect height="11" rx="2" width="13" x="1.5" y="2.5" />
    <path d="M6 2.5v11" />
    <path d="M3.4 5.6h1.2M3.4 8h1.2" />
  </>,
  panelRight: <>
    <rect height="11" rx="2" width="13" x="1.5" y="2.5" />
    <path d="M10 2.5v11" />
    <path d="M11.4 5.6h1.2M11.4 8h1.2" />
  </>,
  bell: <>
    <path d="M4 6.75a4 4 0 0 1 8 0c0 3 .9 4 1.4 4.5H2.6C3.1 10.75 4 9.75 4 6.75Z" />
    <path d="M6.6 13.2a1.6 1.6 0 0 0 2.8 0" />
  </>,
  plus: <path d="M8 3.75v8.5M3.75 8h8.5" />,
  minus: <path d="M3.75 8h8.5" />,
  discard: <>
    <path d="M5.6 3.35 2.6 6.35l3 3" />
    <path d="M2.6 6.35h6.15a3.6 3.6 0 1 1 0 7.2H6.2" />
  </>,
  refresh: <>
    <path d="M12.6 5.9A5 5 0 1 0 13 8" />
    <path d="M9.7 5.9h2.9V3" />
  </>,
  splitRight: <>
    <rect height="11" rx="2" width="13" x="1.5" y="2.5" />
    <path d="M8 2.5v11" />
  </>,
  close: <path d="m4.25 4.25 7.5 7.5M11.75 4.25l-7.5 7.5" />,
  chevronRight: <path d="M6.25 3.75 10.5 8l-4.25 4.25" />,
  chevronDown: <path d="M3.75 6.25 8 10.5l4.25-4.25" />,
  file: <>
    <path d="M4 2.5h4.5L12 6v7.5H4Z" />
    <path d="M8.25 2.6V6H11.9" />
  </>,
  markdown: <>
    <rect height="9.5" rx="1.5" width="13" x="1.5" y="3.25" />
    <path d="M4 10.25v-4l1.75 2 1.75-2v4" />
    <path d="M10.5 6.25v4m0 0-1.25-1.4m1.25 1.4 1.25-1.4" />
  </>,
  diff: <>
    <path d="M4.5 3v6.5M2.5 5h4" />
    <path d="M9.5 11h4" />
  </>,
  zoom: <>
    <path d="M2.75 6V2.75H6M10 2.75h3.25V6M13.25 10v3.25H10M6 13.25H2.75V10" />
  </>,
  // 11.1.3 names both of these. `search` marks the palette's input the way the
  // mock's `⌕` does; `branch` marks the titlebar's branch name, which was a
  // bare string with no way to tell it apart from the workspace name beside it.
  search: <>
    <circle cx="7" cy="7" r="4.25" />
    <path d="m10.2 10.2 3 3" />
  </>,
  branch: <>
    <circle cx="4.75" cy="3.75" r="1.75" />
    <circle cx="4.75" cy="12.25" r="1.75" />
    <circle cx="11.25" cy="3.75" r="1.75" />
    <path d="M4.75 5.5v5M11.25 5.5v1.25a2.5 2.5 0 0 1-2.5 2.5H6.5" />
  </>,
  folder: <path d="M1.75 12.25v-8A1.25 1.25 0 0 1 3 3h2.6l1.4 1.75h6A1.25 1.25 0 0 1 14.25 6v6.25a1.25 1.25 0 0 1-1.25 1.25H3a1.25 1.25 0 0 1-1.25-1.25Z" />,
  // The open state is the same folder with its front panel swung forward, so a
  // directory does not change silhouette when it expands — only its lid moves.
  folderOpen: <>
    <path d="M1.75 12.25v-8A1.25 1.25 0 0 1 3 3h2.6l1.4 1.75h6A1.25 1.25 0 0 1 14.25 6v1.5" />
    <path d="M1.9 12.9 3.7 7.75h10.5l-1.8 5.15a1.25 1.25 0 0 1-1.18.85H3a1.25 1.25 0 0 1-1.1-.85Z" />
  </>,
  fileCode: <path d="m6 4.75-3.25 3.3L6 11.25M10 4.75l3.25 3.3L10 11.25" />,
  fileText: <>
    <path d="M4 2.5h4.5L12 6v7.5H4Z" />
    <path d="M8.25 2.6V6H11.9" />
    <path d="M5.9 8.6h4.2M5.9 10.8h3" />
  </>,
  // Six teeth on a visible hub, not eight rays: at 14px an eight-spoke gear
  // collapses into an asterisk. Checked on the QA render at 14px and 48px.
  fileConfig: <>
    <circle cx="8" cy="8" r="2.4" />
    <path d="M11.5 8h1.9M4.5 8H2.6M9.75 11.03l.95 1.65M6.25 11.03l-.95 1.65M6.25 4.97l-.95-1.65M9.75 4.97l.95-1.65" />
  </>,
  fileImage: <>
    <rect height="9.5" rx="1.5" width="11" x="2.5" y="3.25" />
    <circle cx="6" cy="6.6" r="1" />
    <path d="m2.9 11.9 3.35-3.3 2.2 2.15 2.3-2.25 2.7 2.65" />
  </>,
  fileLock: <>
    <rect height="6.25" rx="1.4" width="9" x="3.5" y="7" />
    <path d="M5.75 7V5.4a2.25 2.25 0 0 1 4.5 0V7" />
  </>,
  fileShell: <path d="m3.25 4.9 3.1 3.1-3.1 3.1M8.4 11.6h4.35" />,
  fileData: <>
    <path d="M6.9 2.9c-1.3 0-1.9.6-1.9 1.9v1.3c0 1.1-.55 1.9-1.5 1.9.95 0 1.5.8 1.5 1.9v1.3c0 1.3.6 1.9 1.9 1.9" />
    <path d="M9.1 2.9c1.3 0 1.9.6 1.9 1.9v1.3c0 1.1.55 1.9 1.5 1.9-.95 0-1.5.8-1.5 1.9v1.3c0 1.3-.6 1.9-1.9 1.9" />
  </>,
};

interface IconProps {
  name: IconName;
  /** Optical size in CSS pixels; the stroke scales with it. */
  size?: number;
  className?: string;
}

export function Icon({ name, size = 14, className }: IconProps) {
  return <svg
    aria-hidden="true"
    className={className ? `icon ${className}` : "icon"}
    fill="none"
    focusable="false"
    height={size}
    stroke="currentColor"
    strokeLinecap="round"
    strokeLinejoin="round"
    strokeWidth={1.4}
    viewBox="0 0 16 16"
    width={size}
  >{paths[name]}</svg>;
}
