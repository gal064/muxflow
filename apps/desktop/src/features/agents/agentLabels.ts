import type { AgentAdapterDescriptor, AgentAdapterId, AgentRecord } from "./types";

const UUID_LIKE = /^(?:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}|[0-9a-f]{24,})$/i;
const GENERIC_TAB_NAME = /^(?:agent|codex|claude(?:-code)?)$/i;
/* Claude Code and Codex prefix their tmux window titles with a status glyph —
   Claude's asterisk frames (· ✢ ✳ ✶ ✻ ✽), Codex's braille spinner frames
   (U+2800–U+28FF), quarter circles (◐ ◓ ◑ ◒), and check/cross marks on
   completion (✓ observed live; the U+2713–U+2718 cluster covers its variants)
   — sometimes with an emoji variation selector. The app draws its own state
   indicators, so the prefix is a second ticker saying the same thing, and the
   primary UI font has no glyph for several of these, which WebKitGTK renders
   as an underscore-like box. Either variation selector counts: a TUI is more
   likely to ask for text presentation (U+FE0E) than emoji, and leaving one
   behind keeps an invisible character that indents the label and defeats the
   generic/UUID tests below. Only these known status sets are stripped; a
   title someone deliberately starts with an emoji keeps it. */
const AGENT_STATUS_GLYPHS = /^(?:[\u00B7\u2713-\u2718\u2722\u2733\u2736\u273B\u273D\u25D0-\u25D3\u2800-\u28FF][\uFE0E\uFE0F]?\s*)+/;

function withoutStatusGlyphs(title: string): string {
  return title.replace(AGENT_STATUS_GLYPHS, "").trim();
}

/** A tmux window title with any leading agent status ticker removed. */
export function stripAgentStatusGlyphs(title: string): string {
  // A title that is nothing but the ticker keeps it: a tab with a blank name
  // is worse than one showing a frame. Callers with a fallback of their own
  // use `withoutStatusGlyphs` and let the empty string fall through instead.
  return withoutStatusGlyphs(title) || title.trim();
}

/** The live tmux tab name, unless it is only a generic or machine identifier. */
export function agentSessionLabel(agent: AgentRecord, adapters: readonly AgentAdapterDescriptor[]): string {
  const tabName = withoutStatusGlyphs(agent.windowName);
  if (tabName && !UUID_LIKE.test(tabName) && !GENERIC_TAB_NAME.test(tabName)) return tabName;
  const assignedName = agent.displayName.trim();
  if (assignedName && !UUID_LIKE.test(assignedName)) return assignedName;
  return adapters.find((adapter) => adapter.id === agent.adapterId)?.displayName
    || adapterFallback(agent.adapterId);
}

function adapterFallback(adapterId: AgentAdapterId): string {
  if (adapterId === "codex") return "Codex";
  if (adapterId === "claude-code" || adapterId === "claude") return "Claude";
  return "Agent";
}
