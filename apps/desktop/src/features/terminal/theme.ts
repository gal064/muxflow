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

/**
 * The chrome tokens this module falls back to with no stylesheet, and the
 * tokens they stand in for. Same contract as `GHOSTTY_DEFAULT_DARK`: these are
 * not a second source of truth, and `theme.test.ts` fails if they ever stop
 * matching `tokens.css`.
 */
export const CHROME_FALLBACKS = {
  "--accent": "#0091ff",
  "--chrome-dim": "#7d848e",
  "--font-mono": '"JetBrains Mono", ui-monospace, "SF Mono", SFMono-Regular, Menlo, monospace',
  "--term-font-size": "13px",
  "--term-line-height": "1.42",
} as const;

export function terminalTheme(root: Element | undefined = globalThis.document?.documentElement): ITheme {
  const read = tokenReader(root);
  const theme: Record<string, string> = {};
  for (const [key, token] of Object.entries(TOKEN_BY_THEME_KEY)) {
    theme[key] = read(token) ?? GHOSTTY_DEFAULT_DARK[key as keyof typeof GHOSTTY_DEFAULT_DARK];
  }
  const slider = read("--chrome-dim") ?? CHROME_FALLBACKS["--chrome-dim"];
  return {
    ...theme,
    scrollbarSliderBackground: `${slider}${SLIDER_ALPHAS.idle}`,
    scrollbarSliderHoverBackground: `${slider}${SLIDER_ALPHAS.hover}`,
    scrollbarSliderActiveBackground: `${slider}${SLIDER_ALPHAS.active}`,
  } as ITheme;
}

/**
 * Search-hit decorations, from the tokens too.
 *
 * xterm draws these itself, so they have to be passed as literal colors rather
 * than picked up from a stylesheet — which is exactly how the last four hex
 * literals in the app survived until they were routed through here. The accent
 * marks the active hit; other hits get the muted chrome ink.
 */
export function searchDecorations(root: Element | undefined = globalThis.document?.documentElement): {
  matchBackground: string;
  matchOverviewRuler: string;
  activeMatchBackground: string;
  activeMatchColorOverviewRuler: string;
} {
  const read = tokenReader(root);
  const accent = read("--accent") ?? CHROME_FALLBACKS["--accent"];
  const muted = read("--chrome-dim") ?? CHROME_FALLBACKS["--chrome-dim"];
  return {
    // Semi-transparent so the glyph underneath stays legible; the overview
    // ruler is a solid 1px mark and cannot be.
    matchBackground: `${muted}66`,
    matchOverviewRuler: muted,
    activeMatchBackground: `${accent}99`,
    activeMatchColorOverviewRuler: accent,
  };
}

/**
 * The terminal's font, from the same tokens the rest of the app uses.
 *
 * `rowPitch` is the token's ratio *applied to the font size* — CSS's meaning of
 * `line-height`, and the meaning every other surface in the app gives
 * `--term-line-height`. It is deliberately not xterm's `lineHeight` option,
 * which multiplies the measured character cell instead; converting between the
 * two is `xtermLineHeight` in `./TerminalRenderer`, and passing this ratio to
 * xterm directly is what rendered 13 px rows at a ~1.86 pitch.
 */
export function terminalFont(root: Element | undefined = globalThis.document?.documentElement): {
  fontFamily: string;
  fontSize: number;
  rowPitch: number;
} {
  const read = tokenReader(root);
  const size = Number.parseFloat(read("--term-font-size") ?? CHROME_FALLBACKS["--term-font-size"]);
  const ratio = Number.parseFloat(read("--term-line-height") ?? CHROME_FALLBACKS["--term-line-height"]);
  const fontSize = Number.isFinite(size) && size > 0 ? size : Number.parseFloat(CHROME_FALLBACKS["--term-font-size"]);
  const lineHeight = Number.isFinite(ratio) && ratio > 0
    ? ratio
    : Number.parseFloat(CHROME_FALLBACKS["--term-line-height"]);
  return {
    fontFamily: read("--font-mono") ?? CHROME_FALLBACKS["--font-mono"],
    fontSize,
    rowPitch: fontSize * lineHeight,
  };
}

/**
 * Every CSS font shorthand the terminal can rasterise with.
 *
 * All four, not just the regular face: bold and italic runs are rasterised into
 * the same texture atlas, and a face that lands after its neighbours splits that
 * atlas between two typefaces just as visibly. One list, because the app waits
 * on these before it mounts and the renderer waits on them again after a bounded
 * timeout — two copies of that list drift, and the drift is invisible until a
 * bold run comes out of a different face than the regular text beside it.
 */
export function terminalFontFaces(root: Element | undefined = globalThis.document?.documentElement): string[] {
  const { fontFamily, fontSize } = terminalFont(root);
  return ["", "700 ", "italic ", "italic 700 "].map((style) => `${style}${fontSize}px ${fontFamily}`);
}

let facesReady: Promise<FontFace[]> | undefined;

/**
 * Resolves once every terminal face is usable, or immediately where the document
 * cannot say. Memoised: each pane asks, and they are all asking about the same
 * four files.
 */
export function terminalFacesReady(): Promise<FontFace[]> {
  facesReady ??= (async () => {
    const fonts = globalThis.document?.fonts;
    if (typeof fonts?.load !== "function") return [];
    return (await Promise.all(terminalFontFaces().map((face) => fonts.load(face)))).flat();
  })();
  return facesReady;
}

export function tokenReader(root: Element | undefined): (token: string) => string | undefined {
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
