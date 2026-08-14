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
  | "back"
  | "forward"
  | "bell"
  | "plus"
  | "splitRight"
  | "splitDown"
  | "close"
  | "chevronRight"
  | "chevronDown"
  | "search"
  | "branch"
  | "terminal"
  | "file"
  | "markdown"
  | "diff"
  | "folder"
  | "zoom";

const paths: Record<IconName, JSX.Element> = {
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
  back: <path d="M9.75 3.5 5.25 8l4.5 4.5" />,
  forward: <path d="M6.25 3.5 10.75 8l-4.5 4.5" />,
  bell: <>
    <path d="M4 6.75a4 4 0 0 1 8 0c0 3 .9 4 1.4 4.5H2.6C3.1 10.75 4 9.75 4 6.75Z" />
    <path d="M6.6 13.2a1.6 1.6 0 0 0 2.8 0" />
  </>,
  plus: <path d="M8 3.75v8.5M3.75 8h8.5" />,
  splitRight: <>
    <rect height="11" rx="2" width="13" x="1.5" y="2.5" />
    <path d="M8 2.5v11" />
  </>,
  splitDown: <>
    <rect height="11" rx="2" width="13" x="1.5" y="2.5" />
    <path d="M1.5 8h13" />
  </>,
  close: <path d="m4.25 4.25 7.5 7.5M11.75 4.25l-7.5 7.5" />,
  chevronRight: <path d="M6.25 3.75 10.5 8l-4.25 4.25" />,
  chevronDown: <path d="M3.75 6.25 8 10.5l4.25-4.25" />,
  search: <>
    <circle cx="7.25" cy="7.25" r="4.25" />
    <path d="m10.5 10.5 3 3" />
  </>,
  branch: <>
    <circle cx="4.5" cy="3.75" r="1.75" />
    <circle cx="4.5" cy="12.25" r="1.75" />
    <circle cx="11.5" cy="3.75" r="1.75" />
    <path d="M4.5 5.5v5M11.5 5.5v1.25a2.5 2.5 0 0 1-2.5 2.5H7a2.5 2.5 0 0 0-2.5 2.5" />
  </>,
  terminal: <>
    <path d="m3.25 5 2.75 3-2.75 3" />
    <path d="M8.25 11.25h4.5" />
  </>,
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
  folder: <path d="M1.75 12.5v-9h4l1.5 2h7v7Z" />,
  zoom: <>
    <path d="M2.75 6V2.75H6M10 2.75h3.25V6M13.25 10v3.25H10M6 13.25H2.75V10" />
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
