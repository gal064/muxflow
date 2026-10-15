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

  // ── Elevation ─────────────────────────────────────────────────────────
  // Shadows and scrims mix this toward transparent at their own strength.
  shadow: "#000000",
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

// ── Light theme (desktop) ───────────────────────────────────────────────
// One Light, the counterpart of the One Dark chrome above, under the same rule:
// the chrome starts at the terminal background and steps toward the ink.
export const lightColors = {
  chromeBg: "#fafafa",
  chromeRaised: "#f2f2f3",
  chromeHairline: "#ececed",
  chromeBorder: "#dbdbde",
  chromeHover: "#efeff0",
  chromeSelected: "#e6e6e8",

  chromeInk: "#383a42",
  chromeInkStrong: "#121317",
  chromeDim: "#696c77",
  chromeFaint: "#a0a1a7",

  accent: "#4078f2",
  accentInk: "#ffffff",
  accentWash: "rgba(64, 120, 242, 0.12)",
  danger: "#e45649",
  dangerInk: "#c13b2f",
  dangerWash: "rgba(228, 86, 73, 0.13)",

  ok: "#2fa84f",
  warn: "#c18401",

  // Softer than black: a light surface needs a fraction of the dark lift.
  shadow: "#16181e59",
} as const satisfies Record<keyof typeof colors, string>;

// One Light's syntax colors, with yellow and cyan from its deeper shades so they
// stay readable on the background. White (7) and bright white (15) are greys
// dark enough to read on the background (GitHub Light's values), because CLIs
// print headings and emphasis in them and a near-white would erase that text.
export const lightTerminalTheme = {
  background: "#fafafa",
  foreground: "#383a42",
  cursor: "#526fff",
  cursorAccent: "#fafafa",
  selectionBackground: "#383a42",
  selectionForeground: "#fafafa",
  black: "#383a42",
  red: "#e45649",
  green: "#50a14f",
  yellow: "#c18401",
  blue: "#4078f2",
  magenta: "#a626a4",
  cyan: "#0184bc",
  white: "#6e7781",
  brightBlack: "#696c77",
  brightRed: "#ca1243",
  brightGreen: "#3d8c3c",
  brightYellow: "#986801",
  brightBlue: "#2f5af3",
  brightMagenta: "#8b2a96",
  brightCyan: "#0997b3",
  brightWhite: "#8c959f",
} as const satisfies Record<keyof typeof terminalTheme, string>;

export const lightStateColors = {
  working: lightTerminalTheme.yellow,
  blocked: lightTerminalTheme.red,
  done: lightTerminalTheme.cyan,
  unknown: lightColors.chromeDim,
} as const satisfies Record<keyof typeof stateColors, string>;


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

