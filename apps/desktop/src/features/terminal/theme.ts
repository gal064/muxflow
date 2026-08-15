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
 * Every non-palette token something outside the stylesheet has to name, and
 * what it is worth when there is no stylesheet to read.
 *
 * Same contract as `GHOSTTY_DEFAULT_DARK`: not a second source of truth, and
 * `theme.test.ts` fails if any entry stops matching `tokens.css`. The list is
 * what is actually read, not every token that exists — an entry nobody reads is
 * an entry nobody notices going stale. Exported for that test alone; everything
 * else goes through `tokenWithFallback`.
 */
export const CHROME_FALLBACKS = {
  "--accent": "#7aa6da",
  "--accent-wash": "#7aa6da1f",
  "--chrome-bg": "#282c34",
  "--chrome-raised": "#2c313a",
  "--chrome-hairline": "#313640",
  "--chrome-border": "#3e4451",
  "--chrome-hover": "#2f343e",
  "--chrome-ink": "#c4c8c6",
  "--chrome-dim": "#8f96a1",
  "--chrome-faint": "#636b78",
  "--font-mono": '"JetBrains Mono", ui-monospace, "SF Mono", SFMono-Regular, Menlo, monospace',
  "--term-font-size": "13px",
  "--term-line-height": "1.42",
} as const;

/** A token `CHROME_FALLBACKS` — and therefore `theme.test.ts` — covers. */
export type FallbackToken = keyof typeof CHROME_FALLBACKS;

/**
 * Reads one of those tokens off a root, or hands back the checked fallback.
 *
 * The reader is what is shared, not the map. Every caller wants the same
 * sentence — "this token, or the value the test pins it to" — and the app had
 * accumulated a copy of it per call site, which is how the editor theme
 * (`../files/monaco`) ended up with twenty-two literals of its own instead. One
 * function means a token can only be read one way, and the `FallbackToken` type
 * means a caller cannot name a token the test does not cover.
 */
export function tokenWithFallback(
  root: Element | undefined = globalThis.document?.documentElement,
): (name: FallbackToken) => string {
  const read = tokenReader(root);
  return (name) => read(name) ?? CHROME_FALLBACKS[name];
}

export function terminalTheme(root: Element | undefined = globalThis.document?.documentElement): ITheme {
  const read = tokenReader(root);
  const theme: Record<string, string> = {};
  for (const [key, token] of Object.entries(TOKEN_BY_THEME_KEY)) {
    theme[key] = read(token) ?? GHOSTTY_DEFAULT_DARK[key as keyof typeof GHOSTTY_DEFAULT_DARK];
  }
  const slider = tokenWithFallback(root)("--chrome-dim");
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
  const token = tokenWithFallback(root);
  const accent = token("--accent");
  const muted = token("--chrome-dim");
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
 * How heavy the terminal's text is allowed to get, as Ghostty answers it.
 *
 * These are xterm option values rather than colours, so they cannot live in
 * `tokens.css`, but they are the same kind of fact as the palette above and
 * they are wrong in the same way when they drift: `bold-is-bright` is off in
 * Ghostty and xterm's `drawBoldTextInBrightColors` defaults to on, so a bold run
 * in the app was being emphasised twice — bold face *and* the bright half of
 * the palette — against a terminal that emphasises it once. The two weights are
 * xterm's own defaults, stated because they are the two the bundled faces
 * actually provide: anything else is synthesised, and a synthesised weight is
 * the other way this terminal has been heavier than its font.
 */
export const GHOSTTY_TEXT_OPTIONS = {
  drawBoldTextInBrightColors: false,
  fontWeight: "normal",
  fontWeightBold: "bold",
} as const;

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
  const token = tokenWithFallback(root);
  // A token that is present but not a number is a case the fallback above
  // cannot catch, so these two are re-checked against the literal after
  // parsing: `--term-font-size: inherit` would otherwise size the grid `NaN`.
  const size = Number.parseFloat(token("--term-font-size"));
  const ratio = Number.parseFloat(token("--term-line-height"));
  const fontSize = Number.isFinite(size) && size > 0 ? size : Number.parseFloat(CHROME_FALLBACKS["--term-font-size"]);
  const lineHeight = Number.isFinite(ratio) && ratio > 0
    ? ratio
    : Number.parseFloat(CHROME_FALLBACKS["--term-line-height"]);
  return {
    fontFamily: token("--font-mono"),
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
let facesSettled = false;

/**
 * Resolves once every terminal face is usable, or immediately where the document
 * cannot say. Memoised: each pane asks, and they are all asking about the same
 * four files.
 *
 * `allSettled`, not `all`: a build missing one of the four — a stripped bundle, a
 * corrupt woff2 — would otherwise reject the whole wait, and the app would fall
 * through to its timeout and mount on the fallback stack. That is the very
 * defect this wait exists to prevent, arrived at from the opposite direction.
 */
export function terminalFacesReady(): Promise<FontFace[]> {
  facesReady ??= (async () => {
    const fonts = globalThis.document?.fonts;
    if (typeof fonts?.load !== "function") return [];
    const settled = await Promise.allSettled(terminalFontFaces().map((face) => fonts.load(face)));
    return settled.flatMap((result) => result.status === "fulfilled" ? result.value : []);
  })().finally(() => { facesSettled = true; });
  return facesReady;
}

/**
 * Whether a terminal face could still arrive after this moment.
 *
 * The renderer needs this to decide whether the glyphs it is about to rasterise
 * can end up disagreeing with the ones it rasterises later. It is a fact about
 * the four faces this module owns, which is why it lives here: asking
 * `document.fonts` directly means scanning every face the document declares and
 * silently assuming the terminal's are the only ones.
 */
export function terminalFacesPending(): boolean {
  return !facesSettled;
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
