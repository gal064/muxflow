/**
 * Design tokens for Muxflow Mobile.
 *
 * Every value here is copied verbatim from `apps/desktop/src/tokens.css`
 * (design.md §10.1). The phone is dark-only in v1, so there is one palette and
 * no theme switch. Nothing else in the app writes a raw colour literal.
 */

export const colors = {
  // ── Chrome surfaces ───────────────────────────────────────────────────
  chromeBg: "#282c34",
  chromeRaised: "#2c313a",
  chromeHairline: "#313640",
  chromeBorder: "#3e4451",
  chromeHover: "#2f343e",
  chromeSelected: "#353b45",

  // ── Chrome ink ────────────────────────────────────────────────────────
  chromeInk: "#c4c8c6",
  chromeInkStrong: "#ffffff",
  chromeDim: "#8f96a1",
  chromeFaint: "#636b78",

  // ── Accent and destructive ────────────────────────────────────────────
  accent: "#7aa6da",
  accentInk: "#282c34",
  accentWash: "rgba(122, 166, 218, 0.12)",
  danger: "#cc6566",
  dangerInk: "#e79596",
  dangerWash: "rgba(204, 101, 102, 0.13)",

  // ── Status ────────────────────────────────────────────────────────────
  ok: "#28c840",
  warn: "#febc2e",
} as const;

/**
 * The terminal palette handed to xterm.js in the WebView (§10.2). `Ghostty
 * Default Style Dark`, ANSI 0-15 included, same as the desktop renderer reads
 * out of `:root`.
 */
export const terminalTheme = {
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
} as const;

/** Agent lifecycle colours (`--state-*` in tokens.css). */
export const stateColors = {
  working: terminalTheme.yellow,
  blocked: terminalTheme.red,
  done: terminalTheme.cyan,
  unknown: colors.chromeDim,
} as const;

/**
 * System sans for chrome, JetBrains Mono for the terminal, paths, keys and the
 * plain file view. Android has no font asset named `System`, so that value
 * falls through to the platform default typeface, which is Roboto — the face
 * §10.1 asks for. `mono` is the family name registered by the `expo-font`
 * plugin block in `app.json`; the plugin emits a `ReactFontManager`
 * registration for it, so `fontWeight: "700"` picks the Bold face.
 */
export const fonts = {
  sans: "System",
  mono: "JetBrains Mono",
} as const;

/** Heights in dp, from the per-screen specs in §9. */
export const metrics = {
  appBarHeight: 56,
  connectionStripHeight: 28,
  tabBarHeight: 56,
  terminalHeaderHeight: 48,
  keyChipRowHeight: 40,
  inputBarHeight: 56,
  hostRowHeight: 72,
  agentRowHeight: 76,
  agentAvatarSize: 36,
  sessionRowHeight: 64,
  windowRowHeight: 64,
  actionRowHeight: 48,
  fileRowHeight: 52,
  attentionEdgeBarWidth: 3,
  hairlineWidth: 1,
} as const;

/** Corner radii in dp (§10.1): 8 on cards and pills, 12 on sheets. */
export const radii = {
  card: 8,
  pill: 8,
  sheet: 12,
} as const;

/** Type scale in sp, from the per-screen specs in §9. */
export const typeScale = {
  appBarTitle: 17,
  rowTitle: 16,
  body: 14,
  rowSecondary: 13,
  meta: 12,
  keyMono: 11,
  terminal: 13,
} as const;

/**
 * Text inside fixed-height chrome may follow Android's font scale up to 1.3×.
 * Beyond that, a 28–56 dp control cannot grow with its label, so React Native
 * clips or paints the label into the neighbouring row. Reading surfaces and
 * dialog copy remain uncapped.
 */
export const fixedChromeText = { maxFontSizeMultiplier: 1.3 } as const;

export const tokens = {
  colors,
  terminalTheme,
  stateColors,
  fonts,
  metrics,
  radii,
  typeScale,
  fixedChromeText,
} as const;

export default tokens;
