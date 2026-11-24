import { lightTerminalTheme, terminalTheme } from "@muxflow/client-core";

import type { SessionState } from "../../store/sessionStore";
import { isCodexAdapter } from "../agents/agentIconPaths";

/**
 * Which palette the terminal is drawn in. It follows the colors the host
 * reports to programs that ask (OSC 10/11), not the phone's appearance: a
 * program such as Codex paints explicit backgrounds for the palette it was
 * told about, and they are only readable on that palette.
 */
export type TerminalScheme = "dark" | "light";

/** Before a host reports colors, and for an older host that never does. */
export const DEFAULT_TERMINAL_SCHEME: TerminalScheme = "dark";

/**
 * The scheme one pane is drawn in. Only Codex reads the reported colors and
 * paints for them, so only a terminal running it takes the host's scheme;
 * every other terminal stays in the app's dark. A pane not known yet is dark
 * too, so a chat pane's brief terminal before it opens as its chat does not
 * flash light, and a pane goes back to dark when Codex leaves its shell.
 */
export function paneTerminalScheme(state: Pick<SessionState, "agents" | "panes" | "terminalScheme">, paneId: string): TerminalScheme {
  const pane = state.panes[paneId];
  if (!pane || pane.chatAdapterId !== "") return DEFAULT_TERMINAL_SCHEME;
  const codex = Object.values(state.agents).some((agent) => agent.present && agent.route.paneId === paneId && isCodexAdapter(agent.adapterId));
  return codex ? state.terminalScheme : DEFAULT_TERMINAL_SCHEME;
}

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
