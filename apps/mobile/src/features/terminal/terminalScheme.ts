import { lightTerminalTheme, terminalTheme } from "@muxflow/client-core";

/**
 * Which palette the terminal is drawn in. It follows the colors the host
 * reports to programs that ask (OSC 10/11), not the phone's appearance: a
 * program such as Codex paints explicit backgrounds for the palette it was
 * told about, and they are only readable on that palette.
 */
export type TerminalScheme = "dark" | "light";

/** Before a host reports colors, and for an older host that never does. */
export const DEFAULT_TERMINAL_SCHEME: TerminalScheme = "dark";

/** The desktop's light terminal is exactly `lightTerminalTheme`. */
export function terminalThemeFor(scheme: TerminalScheme): typeof terminalTheme | typeof lightTerminalTheme {
  return scheme === "light" ? lightTerminalTheme : terminalTheme;
}

/**
 * The scheme for a reported `#rrggbb` background, by the same perceived
 * brightness test Codex uses to pick its own colors (`is_light`, codex-rs
 * `tui/src/color.rs`), so the two never disagree about a background.
 */
export function schemeForBackground(hex: string): TerminalScheme {
  const match = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!match) return DEFAULT_TERMINAL_SCHEME;
  const value = Number.parseInt(match[1]!, 16);
  const brightness = 0.299 * ((value >> 16) & 0xff) + 0.587 * ((value >> 8) & 0xff) + 0.114 * (value & 0xff);
  return brightness > 128 ? "light" : "dark";
}
