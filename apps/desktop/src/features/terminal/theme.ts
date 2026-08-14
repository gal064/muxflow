import type { ITheme } from "@xterm/xterm";

/**
 * The terminal's colors, taken from the same token file the chrome reads.
 *
 * Before Phase 11 this palette lived as a second, divergent set of hex literals
 * inside the renderer, which is how the app ended up with a terminal that
 * matched nothing else on screen. The values now live in `tokens.css` as
 * `--term-*` custom properties; this module reads them off `:root` at
 * construction time so there is exactly one place to change a color.
 *
 * `GHOSTTY_DEFAULT_DARK` is the fallback for environments with no stylesheet
 * (the headless test runner, and the instant between document creation and the
 * first style resolve). It is not a second source of truth: `theme.test.ts`
 * parses `tokens.css` and fails if the two disagree.
 */
export const GHOSTTY_DEFAULT_DARK = {
  background: "#282c34",
  foreground: "#ffffff",
  cursor: "#ffffff",
  cursorAccent: "#353a44",
  selectionBackground: "#ffffff",
  selectionForeground: "#282c34",
  black: "#1d1f21",
  red: "#cc6566",
  green: "#b6bd68",
  yellow: "#f0c674",
  blue: "#82a2be",
  magenta: "#b294bb",
  cyan: "#8abeb7",
  white: "#c4c8c6",
  brightBlack: "#666666",
  brightRed: "#d54e53",
  brightGreen: "#b9ca4b",
  brightYellow: "#e7c547",
  brightBlue: "#7aa6da",
  brightMagenta: "#c397d8",
  brightCyan: "#70c0b1",
  brightWhite: "#eaeaea",
} as const satisfies Record<string, string>;

/** Which `--term-*` token backs each xterm theme key. */
const TOKEN_BY_THEME_KEY: Record<keyof typeof GHOSTTY_DEFAULT_DARK, string> = {
  background: "--term-bg",
  foreground: "--term-fg",
  cursor: "--term-cursor",
  cursorAccent: "--term-cursor-text",
  selectionBackground: "--term-selection-bg",
  selectionForeground: "--term-selection-fg",
  black: "--term-0",
  red: "--term-1",
  green: "--term-2",
  yellow: "--term-3",
  blue: "--term-4",
  magenta: "--term-5",
  cyan: "--term-6",
  white: "--term-7",
  brightBlack: "--term-8",
  brightRed: "--term-9",
  brightGreen: "--term-10",
  brightYellow: "--term-11",
  brightBlue: "--term-12",
  brightMagenta: "--term-13",
  brightCyan: "--term-14",
  brightWhite: "--term-15",
};

/**
 * Scrollbar sliders are chrome inside the terminal, so they are the one part of
 * the terminal surface that is deliberately *not* Ghostty: they derive from the
 * chrome ink at fixed alphas, the same way every other chrome affordance does.
 */
const SLIDER_ALPHAS = { idle: "40", hover: "66", active: "99" } as const;

export function terminalTheme(root: Element | undefined = globalThis.document?.documentElement): ITheme {
  const read = tokenReader(root);
  const theme: Record<string, string> = {};
  for (const [key, token] of Object.entries(TOKEN_BY_THEME_KEY)) {
    theme[key] = read(token) ?? GHOSTTY_DEFAULT_DARK[key as keyof typeof GHOSTTY_DEFAULT_DARK];
  }
  const slider = read("--chrome-dim") ?? "#7d848e";
  return {
    ...theme,
    scrollbarSliderBackground: `${slider}${SLIDER_ALPHAS.idle}`,
    scrollbarSliderHoverBackground: `${slider}${SLIDER_ALPHAS.hover}`,
    scrollbarSliderActiveBackground: `${slider}${SLIDER_ALPHAS.active}`,
  } as ITheme;
}

/** The terminal's font, from the same tokens the rest of the app uses. */
export function terminalFont(root: Element | undefined = globalThis.document?.documentElement): {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
} {
  const read = tokenReader(root);
  const size = Number.parseFloat(read("--term-font-size") ?? "");
  const height = Number.parseFloat(read("--term-line-height") ?? "");
  return {
    fontFamily: read("--font-mono") ?? '"JetBrains Mono", ui-monospace, "SF Mono", SFMono-Regular, Menlo, monospace',
    fontSize: Number.isFinite(size) && size > 0 ? size : 13,
    lineHeight: Number.isFinite(height) && height > 0 ? height : 1.42,
  };
}

function tokenReader(root: Element | undefined): (token: string) => string | undefined {
  if (!root || typeof globalThis.getComputedStyle !== "function") return () => undefined;
  let style: CSSStyleDeclaration;
  try {
    style = globalThis.getComputedStyle(root);
  } catch {
    return () => undefined;
  }
  return (token) => {
    const value = style.getPropertyValue(token).trim();
    return value.length > 0 ? value : undefined;
  };
}
