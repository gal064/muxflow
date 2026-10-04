// Shared palette; desktop CSS is generated from this source. Layout stays platform-specific.
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

export const stateColors = {
  working: terminalTheme.yellow,
  blocked: terminalTheme.red,
  done: terminalTheme.cyan,
  unknown: colors.chromeDim,
} as const;


export const terminalCssTokens: Record<keyof typeof terminalTheme, string> = {
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

