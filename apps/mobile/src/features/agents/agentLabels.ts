// Ported from apps/desktop/src/features/agents/agentLabels.ts (§8.2). Only the
// glyph stripping is needed on the phone.

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

function withoutStatusGlyphs(title: string): string {
  return title.replace(AGENT_STATUS_GLYPHS, "").trim();
}

/** A tmux window title with any leading agent status ticker removed. */
export function stripAgentStatusGlyphs(title: string): string {
  // A title that is nothing but the ticker keeps it: a tab with a blank name
  // is worse than one showing a frame.
  return withoutStatusGlyphs(title) || title.trim();
}
