import { describe, expect, it } from "vitest";
import { agentSessionLabel, stripAgentStatusGlyphs, type SessionLabelInput } from "./agentLabels";

// Ported verbatim from apps/desktop/src/features/agents/agentLabels.test.ts.
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

function agent(overrides: Partial<SessionLabelInput>): SessionLabelInput {
  return { windowName: "", displayName: "Agent 1", adapterId: "claude-code", ...overrides };
}

describe("agentSessionLabel (the row's name: the tab, not the adapter)", () => {
  it("labels an agent by its tab name with the ticker removed", () => {
    expect(agentSessionLabel(agent({ windowName: "✳ Plan rollout" }), [])).toBe("Plan rollout");
    // Stripping happens before the generic-name test, so a ticker plus a
    // generic word still falls through to the assigned name.
    expect(agentSessionLabel(agent({ windowName: "⠦ codex", displayName: "Nightly triage" }), []))
      .toBe("Nightly triage");
  });

  it("falls through a title that is only a ticker frame", () => {
    // A starting agent's title is the spinner frame alone. A tab has nothing
    // better to show and keeps it; a row has the assigned name.
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "Nightly triage" }), []))
      .toBe("Nightly triage");
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "", adapterId: "codex" }),
      [{ id: "codex", displayName: "Codex CLI" }])).toBe("Codex CLI");
    // And to the built-in name when the host named no adapters at all.
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "", adapterId: "codex" }), []))
      .toBe("Codex");
    expect(agentSessionLabel(agent({ windowName: "", displayName: "", adapterId: "claude-code" }), [])).toBe("Claude");
    expect(agentSessionLabel(agent({ windowName: "", displayName: "", adapterId: "claude" }), [])).toBe("Agent");
    expect(agentSessionLabel(agent({ windowName: "", displayName: "", adapterId: "other" }), [])).toBe("Agent");
  });

  it("rejects generic and machine tab names, case-insensitively", () => {
    for (const generic of ["agent", "Codex", "claude", "Claude-Code", "CLAUDE"]) {
      expect(agentSessionLabel(agent({ windowName: generic, displayName: "Fix tests" }), []), generic).toBe("Fix tests");
    }
    expect(agentSessionLabel(agent({ windowName: "01a03cb0-7122-7410-a867-f5c95d64faa5", displayName: "Fix tests" }), []))
      .toBe("Fix tests");
    expect(agentSessionLabel(agent({ windowName: "0123456789abcdef0123456789abcdef", displayName: "Fix tests" }), []))
      .toBe("Fix tests");
    // A UUID for an assigned name falls through too.
    expect(agentSessionLabel(agent({ windowName: "codex", displayName: "01a03cb0-7122-7410-a867-f5c95d64faa5", adapterId: "codex" }), []))
      .toBe("Codex");
    // But a name that merely contains one is a name.
    expect(agentSessionLabel(agent({ windowName: "Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5" }), []))
      .toBe("Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5");
  });
});
