// Ported from apps/desktop/src/features/agents/agentLabels.ts (§8.2, §9.3.1).

import type { Agent, AgentAdapterDescriptor } from "../../store/sessionStore";

const UUID_LIKE = /^(?:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}|[0-9a-f]{24,})$/i;
const GENERIC_TAB_NAME = /^(?:agent|codex|claude(?:-code)?)$/i;
/* Claude Code and Codex prefix their tmux window titles with a status marker:
   Claude's asterisk frames (U+00B7, U+2722, U+2733, U+2736, U+273B, U+273D),
   Codex's braille spinner frames (U+2800-U+28FF), quarter circles
   (U+25D0-U+25D3), and check/cross marks on completion (the U+2713-U+2718
   cluster), plus its exact `[ . ]` / `[ ! ]` action-required frames. Glyph
   markers sometimes carry an emoji variation selector (U+FE0E / U+FE0F). The
   app draws its own state indicators, so the prefix is a second ticker saying
   the same thing. Only these known status sets are stripped; a title someone
   deliberately starts with an emoji keeps it. */
const AGENT_STATUS_GLYPHS = new RegExp(
  "^(?:(?:\\[ [.!] \\]|[\\u00B7\\u2713-\\u2718\\u2722\\u2733\\u2736\\u273B\\u273D\\u25D0-\\u25D3\\u2800-\\u28FF][\\uFE0E\\uFE0F]?)\\s*)+",
);

/** `stripAgentStatusGlyphs` without its keep-the-frame rule: empty when the title was only a ticker, for callers with a fallback of their own. */
export function withoutStatusGlyphs(title: string): string {
  return title.replace(AGENT_STATUS_GLYPHS, "").trim();
}

/** A tmux window title with any leading agent status ticker removed. */
export function stripAgentStatusGlyphs(title: string): string {
  // A title that is nothing but the ticker keeps it: a tab with a blank name
  // is worse than one showing a frame. Callers with a fallback of their own
  // use `withoutStatusGlyphs` and let the empty string fall through instead.
  return withoutStatusGlyphs(title) || title.trim();
}

/** What `agentSessionLabel` reads: the desktop's `AgentRecord` carries `windowName` itself; here the caller resolves it from the topology. */
export type SessionLabelInput = Pick<Agent, "displayName" | "adapterId"> & { windowName: string };

/** The live tmux tab name, unless it is only a generic or machine identifier. */
export function agentSessionLabel(agent: SessionLabelInput, adapters: readonly Pick<AgentAdapterDescriptor, "id" | "displayName">[]): string {
  const tabName = withoutStatusGlyphs(agent.windowName);
  if (tabName && !UUID_LIKE.test(tabName) && !GENERIC_TAB_NAME.test(tabName)) return tabName;
  const assignedName = agent.displayName.trim();
  if (assignedName && !UUID_LIKE.test(assignedName)) return assignedName;
  return adapters.find((adapter) => adapter.id === agent.adapterId)?.displayName
    || adapterFallback(agent.adapterId);
}

function adapterFallback(adapterId: string): string {
  if (adapterId === "codex") return "Codex";
  if (adapterId === "claude-code") return "Claude";
  return "Agent";
}
