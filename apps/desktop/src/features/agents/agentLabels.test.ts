import { describe, expect, it } from "vitest";
import { agentSessionLabel, stripAgentStatusGlyphs } from "./agentLabels";
import { agent } from "./testFixtures";
import type { AgentAdapterDescriptor } from "./types";

describe("agent status glyph stripping", () => {
  // Claude Code and Codex animate a status glyph at the front of their tmux
  // window titles. The app draws its own state indicators, and the primary UI
  // font lacks several of these glyphs (WebKitGTK renders them as an
  // underscore-like box), so labels drop the ticker rather than render it.
  it("removes each agent status ticker the CLIs actually emit", () => {
    for (const glyph of ["·", "✢", "✳", "✶", "✻", "✽", "◐", "◓", "◑", "◒", "⠦", "⠋", "⣷", "✓", "✗"]) {
      expect(stripAgentStatusGlyphs(`${glyph} Fix tests`), glyph).toBe("Fix tests");
    }
    // The emoji presentation variant Claude sometimes emits (✳ + U+FE0F).
    expect(stripAgentStatusGlyphs("✳️ Fix tests")).toBe("Fix tests");
    // Text presentation (U+FE0E) is the one a TUI is likelier to ask for, and
    // leaving it behind indents the label with an invisible character.
    expect(stripAgentStatusGlyphs("✳︎ Fix tests")).toBe("Fix tests");
    // Consecutive frames left behind by a fast redraw still count as one prefix.
    expect(stripAgentStatusGlyphs("✳ ✶ Fix tests")).toBe("Fix tests");
    // Codex's completion title, exactly as sampled from a live tmux server.
    expect(stripAgentStatusGlyphs("✓ Update customize.py for oma… · gpt-5.6-sol · ~/dev"))
      .toBe("Update customize.py for oma… · gpt-5.6-sol · ~/dev");
  });

  it("leaves deliberate names alone", () => {
    // A title someone starts with an ordinary emoji is a name, not a ticker.
    expect(stripAgentStatusGlyphs("🚀 deploy")).toBe("🚀 deploy");
    expect(stripAgentStatusGlyphs("Fix tests")).toBe("Fix tests");
    // Glyphs after the first word are content, not status.
    expect(stripAgentStatusGlyphs("release ✳ notes")).toBe("release ✳ notes");
    // A title that is nothing but the glyph keeps it rather than going blank.
    expect(stripAgentStatusGlyphs("✳")).toBe("✳");
  });

  it("labels an agent by its tab name with the ticker removed", () => {
    expect(agentSessionLabel(agent({ windowName: "✳ Plan rollout" }), [])).toBe("Plan rollout");
    // Stripping happens before the generic-name test, so a ticker plus a
    // generic word still falls through to the assigned name.
    expect(agentSessionLabel(agent({ windowName: "⠦ codex", displayName: "Nightly triage" }), []))
      .toBe("Nightly triage");
  });

  it("falls through a title that is only a ticker frame", () => {
    // A starting agent's title is the spinner frame alone. A tab has nothing
    // better to show and keeps it; a sidebar row has the assigned name.
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "Nightly triage" }), []))
      .toBe("Nightly triage");
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "", adapterId: "codex" }),
      [{ id: "codex", displayName: "Codex CLI" }] as AgentAdapterDescriptor[])).toBe("Codex CLI");
    // And to the built-in name when the host named no adapters at all.
    expect(agentSessionLabel(agent({ windowName: "⠋", displayName: "", adapterId: "codex" }), []))
      .toBe("Codex");
  });
});
