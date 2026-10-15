// Shared palette; desktop CSS is generated from this source. Layout stays platform-specific.
export const colors = {
  // ── Chrome surfaces ───────────────────────────────────────────────────
  chromeBg: "#282c34",
  chromeRaised: "#2c313a",
  chromeHairline: "#313640",
  chromeBorder: "#3e4451",
  chromeHover: "#2f343e",
  chromeSelected: "#353b45",
  // A selected row's edge. Dark fills a selected row a step lighter and needs no
  // edge, so it is the fill itself; light draws white rows that need one.
  chromeSelectedEdge: "#353b45",

  // ── Chrome ink ────────────────────────────────────────────────────────
  chromeInk: "#c4c8c6",
  chromeInkStrong: "#ffffff",
  // Raised from #8f96a1 and #636b78, which drew hints and labels at 2.4:1 on a
  // dialog. Faint stays a step below dim, so the two still read as two levels.
  chromeDim: "#a4abb6",
  chromeFaint: "#7f8794",

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
// Modelled on T3 Code's light theme: neutral zinc greys rather than One Light's
// blue-tinted ones, the work area a shade whiter than the chrome around it,
// and white selected rows set off by a border.
export const lightColors = {
  chromeBg: "#fafafa",
  // Raised stays a grey a step below the ground, not white: borderless code
  // blocks, inline code and table stripes are drawn in it.
  chromeRaised: "#f4f4f5",
  chromeHairline: "#e4e4e7",
  chromeBorder: "#d4d4d8",
  chromeHover: "#ececee",
  // White with a border, so a selected row is a different shape from a hovered
  // one rather than a lighter shade of it.
  chromeSelected: "#ffffff",
  chromeSelectedEdge: "#d4d4d8",

  chromeInk: "#27272a",
  chromeInkStrong: "#18181b",
  chromeDim: "#71717b",
  // Darker than zinc's #9f9fa9, which drew section labels at 2.5:1.
  chromeFaint: "#85858e",

  accent: "#2f6fe0",
  accentInk: "#ffffff",
  accentWash: "rgba(47, 111, 224, 0.12)",
  danger: "#dc2626",
  dangerInk: "#b91c1c",
  dangerWash: "rgba(220, 38, 38, 0.13)",

  ok: "#16a34a",
  warn: "#b45309",

  // Softer than black: a light surface needs a fraction of the dark lift.
  shadow: "#18181b40",
} as const satisfies Record<keyof typeof colors, string>;

// VS Code's light terminal colours, which T3 Code uses too: red, green, blue
// and magenta read at 5:1 or better, yellow and cyan at about 3:1 as they do in
// VS Code. Its bright green, yellow and white are near-invisible on a light
// background (about 2:1), and CLIs print emphasis in them, so those three are
// darkened here; white (7) is a grey for the same reason.
export const lightTerminalTheme = {
  background: "#fcfcfc",
  foreground: "#1c2129",
  cursor: "#26384e",
  cursorAccent: "#fcfcfc",
  selectionBackground: "#253f6333",
  selectionForeground: "#1c2129",
  black: "#000000",
  red: "#cd3131",
  green: "#107c10",
  yellow: "#949800",
  blue: "#0451a5",
  magenta: "#bc05bc",
  cyan: "#0598bc",
  white: "#555555",
  brightBlack: "#666666",
  brightRed: "#cd3131",
  brightGreen: "#0e8a0e",
  brightYellow: "#7d8000",
  brightBlue: "#0451a5",
  brightMagenta: "#bc05bc",
  brightCyan: "#0598bc",
  brightWhite: "#767676",
} as const satisfies Record<keyof typeof terminalTheme, string>;

export const lightStateColors = {
  working: "#d97706",
  blocked: lightColors.danger,
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

