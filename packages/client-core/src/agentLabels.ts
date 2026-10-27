export interface AgentLabelInput {
  windowName: string;
  /** The agent pane's own title; empty from a host that predates it. */
  paneTitle: string;
  displayName: string;
  adapterId: string;
}

export interface AgentLabelAdapter {
  id: string;
  displayName: string;
}

const UUID_LIKE = /^(?:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}|[0-9a-f]{24,})$/i;
const GENERIC_TAB_NAME = /^(?:agent|codex|claude(?:[- ]code)?|pi)$/i;
/* Claude Code and Codex prefix their tmux window titles with a status marker —
   Claude's asterisk frames (· ✢ ✳ ✶ ✻ ✽), Codex's braille spinner frames
   (U+2800–U+28FF), quarter circles (◐ ◓ ◑ ◒), and check/cross marks on
   completion (✓ observed live; the U+2713–U+2718 cluster covers its variants),
   plus its exact `[ . ]` / `[ ! ]` action-required frames. Glyph markers
   sometimes carry an emoji variation selector. The app draws its own state
   indicators, so the prefix is a second ticker saying the same thing, and the
   primary UI font has no glyph for several of these, which WebKitGTK renders
   as an underscore-like box. Either variation selector counts: a TUI is more
   likely to ask for text presentation (U+FE0E) than emoji, and leaving one
   behind keeps an invisible character that indents the label and defeats the
   generic/UUID tests below. pi's title starts with its own name instead,
   `π - <session> - <dir>`, which the adapter mark already says. Only these
   known prefixes are stripped; a title someone deliberately starts with an
   emoji keeps it. */
const AGENT_STATUS_GLYPHS = /^(?:(?:\[ [.!] \]|π -|[\u00B7\u2713-\u2718\u2722\u2733\u2736\u273B\u273D\u25D0-\u25D3\u2800-\u28FF][\uFE0E\uFE0F]?)\s*)+/;

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

/** The agent pane's own title, then the live tmux tab name, unless either is
    only a generic or machine identifier. The pane comes first because a window
    is named after one pane: agents sharing a split, or a pane moved into a tab
    of its own, would otherwise all read the same name or the generic one. */
export function agentSessionLabel(agent: AgentLabelInput, adapters: readonly AgentLabelAdapter[]): string {
  for (const title of [agent.paneTitle, agent.windowName]) {
    const name = withoutStatusGlyphs(title);
    if (name && !UUID_LIKE.test(name) && !GENERIC_TAB_NAME.test(name)) return name;
  }
  const assignedName = agent.displayName.trim();
  if (assignedName && !UUID_LIKE.test(assignedName)) return assignedName;
  return adapters.find((adapter) => adapter.id === agent.adapterId)?.displayName
    || adapterFallback(agent.adapterId);
}

function adapterFallback(adapterId: string): string {
  if (adapterId === "codex") return "Codex";
  if (adapterId === "claude-code") return "Claude";
  if (adapterId === "pi") return "Pi";
  return "Agent";
}
