import { describe, expect, it } from "vitest";
import { stripAgentStatusGlyphs } from "./agentLabels";

// Ported verbatim from apps/desktop/src/features/agents/agentLabels.test.ts
// (the stripping cases; the label fallback chain is desktop-only).
describe("agent status glyph stripping", () => {
  it("removes each agent status ticker the CLIs actually emit", () => {
    for (const glyph of ["·", "✢", "✳", "✶", "✻", "✽", "◐", "◓", "◑", "◒", "⠦", "⠋", "⣷", "✓", "✗", "[ . ]", "[ ! ]"]) {
      expect(stripAgentStatusGlyphs(`${glyph} Fix tests`), glyph).toBe("Fix tests");
    }
    expect(stripAgentStatusGlyphs("✳️ Fix tests")).toBe("Fix tests");
    expect(stripAgentStatusGlyphs("✳︎ Fix tests")).toBe("Fix tests");
    expect(stripAgentStatusGlyphs("✳ ✶ Fix tests")).toBe("Fix tests");
    expect(stripAgentStatusGlyphs("✓ Update customize.py for oma… · gpt-5.6-sol · ~/dev"))
      .toBe("Update customize.py for oma… · gpt-5.6-sol · ~/dev");
    expect(stripAgentStatusGlyphs("[ . ] Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5"))
      .toBe("Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5");
    expect(stripAgentStatusGlyphs("[ ! ] Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5"))
      .toBe("Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5");
  });

  it("leaves deliberate names alone", () => {
    expect(stripAgentStatusGlyphs("🚀 deploy")).toBe("🚀 deploy");
    expect(stripAgentStatusGlyphs("Fix tests")).toBe("Fix tests");
    expect(stripAgentStatusGlyphs("release ✳ notes")).toBe("release ✳ notes");
    expect(stripAgentStatusGlyphs("✳")).toBe("✳");
  });
});
