import { invoke } from "@tauri-apps/api/core";
import { terminalTheme } from "./theme";

/**
 * Tells every host which colors the terminal is drawn with.
 *
 * tmux answers a program's "what is your background?" query (OSC 10/11) on
 * the terminal's behalf, from what the host reports. Without this, a light
 * terminal is described as dark, and Codex, Neovim and bat pick palettes for a
 * background that is not there. A failure only leaves the host on its dark
 * default, so it is logged and never surfaced.
 */
export function reportTerminalColorsToHosts(): void {
  const { foreground, background } = terminalTheme();
  void invoke("set_terminal_colors", { foreground, background })
    .catch((error: unknown) => console.warn("could not report terminal colors to the host", error));
}
