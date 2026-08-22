import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AgentIcon, AgentMark } from "./AgentIdentity";
import { agentSessionLabel } from "./agentLabels";
import { agent } from "./testFixtures";
import type { AgentAdapterDescriptor } from "./types";

const adapters = [
  { id: "codex", displayName: "Codex" },
  { id: "claude-code", displayName: "Claude Code" },
] as AgentAdapterDescriptor[];

describe("agent session presentation", () => {
  it("uses the live tab name and leaves long names intact for CSS truncation and the title", () => {
    const long = "Investigate the production authentication timeout without losing context";
    expect(agentSessionLabel(agent({ windowName: long }), adapters)).toBe(long);
  });

  it("hides UUID-like tab names behind the adapter label", () => {
    expect(agentSessionLabel(agent({ windowName: "550e8400-e29b-41d4-a716-446655440000", displayName: "550e8400-e29b-41d4-a716-446655440000" }), adapters)).toBe("Codex");
    expect(agentSessionLabel(agent({ adapterId: "claude-code", windowName: "00000000-0000-0000-0000-000000000000", displayName: "Review auth flow" }), adapters)).toBe("Review auth flow");
    expect(agentSessionLabel(agent({ adapterId: "future", windowName: "0123456789abcdef01234567", displayName: "0123456789abcdef01234567" }), [])).toBe("Agent");
  });

  it("renders distinct decorative Codex and Claude marks", () => {
    const codex = renderToStaticMarkup(<AgentIcon adapterId="codex" />);
    expect(codex).toContain('data-icon-source="openai-blossom"');
    expect(codex).toContain('viewBox="146.694 227.042 267.198 264.812"');
    expect(codex).toContain("M249.176 323.434V298.276");
    expect(renderToStaticMarkup(<AgentIcon adapterId="claude-code" />)).toContain('data-agent-icon="claude"');
  });

  it("matches the two marks by weight rather than by box", () => {
    // The blossom fills its viewBox edge to edge and Claude's mark is strokes
    // inside a 16-unit box, so at one shared size the filled mark carried
    // visibly more ink. The inset is applied to the *wrapper*, about the
    // viewBox's own centre — the official path is untouched, which is what
    // `data-icon-source` promises.
    const codex = renderToStaticMarkup(<AgentIcon adapterId="codex" />);
    expect(codex).toContain('<g transform="translate(280.293 359.448) scale(.86) translate(-280.293 -359.448)">');
    expect(codex).toContain("M249.176 323.434V298.276C249.176 296.158 249.971 294.569 251.825 293.509");
    // Four spokes, not eight: at 12px the eight-spoke asterisk filled its own
    // centre and read as a blot. Weight comes from the stroke width in CSS.
    const claude = renderToStaticMarkup(<AgentIcon adapterId="claude-code" />);
    expect(claude).toContain('d="M8 1.5v13M1.5 8h13M3.4 3.4l9.2 9.2M12.6 3.4l-9.2 9.2"');
    expect(claude.match(/M/g)).toHaveLength(4);
  });

  it("docks the state to the mark, and steps aside for the glyph option", () => {
    const badged = renderToStaticMarkup(<AgentMark adapterId="codex" glyphs={false} state="blocked" />);
    expect(badged).toContain('class="agent-mark"');
    expect(badged).toContain('data-agent-icon="codex"');
    expect(badged).toContain('class="agent-mark-badge blocked"');
    // Working is a process, so the badge spins — the same `spinner` class the
    // tab strip and the workspace title use, so there is one ring in the app.
    expect(renderToStaticMarkup(<AgentMark adapterId="codex" glyphs={false} state="working" />))
      .toContain('class="spinner agent-mark-badge working"');
    // A 6px badge cannot hold a legible glyph, and the option exists precisely
    // so state does not depend on colour: the full-size dot comes back instead
    // of a shrunken one, in front of the mark as before.
    const glyphed = renderToStaticMarkup(<AgentMark adapterId="codex" glyphs state="working" />);
    expect(glyphed).toContain('class="state-dot working glyphs"');
    expect(glyphed).not.toContain("agent-mark-badge");
    expect(glyphed.indexOf("state-dot")).toBeLessThan(glyphed.indexOf("agent-icon"));
    // Still one node, though. A bare fragment handed the dot and the icon
    // separately to the compact workspace cluster, whose flex row has a single
    // uniform gap, so three agents rendered as six equally spaced marks with
    // nothing to say which dot belonged to which icon.
    expect(glyphed.startsWith('<span class="agent-mark agent-mark-glyphs">')).toBe(true);
    expect(glyphed.endsWith("</span>")).toBe(true);
    expect(glyphed.indexOf('class="agent-mark')).toBe(glyphed.lastIndexOf('class="agent-mark'));
  });
});
