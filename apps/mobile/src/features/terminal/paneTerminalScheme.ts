import type { SessionState } from "../../store/sessionStore";
import { isCodexAdapter } from "../agents/agentIconPaths";
import { DEFAULT_TERMINAL_SCHEME, type TerminalScheme } from "./terminalScheme";

/**
 * The scheme one pane is drawn in. Only Codex reads the reported colors and
 * paints for them, so only a terminal running it takes the host's scheme;
 * every other terminal stays in the app's dark. A pane not known yet is dark
 * too, so a chat pane's brief terminal before it opens as its chat does not
 * flash light, and a pane goes back to dark when Codex leaves its shell.
 *
 * Kept apart from `terminalScheme.ts`, which the WebView page bundles.
 */
export function paneTerminalScheme(state: Pick<SessionState, "agents" | "panes" | "terminalScheme">, paneId: string): TerminalScheme {
  const pane = state.panes[paneId];
  if (!pane || pane.chatAdapterId !== "") return DEFAULT_TERMINAL_SCHEME;
  const codex = Object.values(state.agents).some((agent) => agent.present && agent.route.paneId === paneId && isCodexAdapter(agent.adapterId));
  return codex ? state.terminalScheme : DEFAULT_TERMINAL_SCHEME;
}
