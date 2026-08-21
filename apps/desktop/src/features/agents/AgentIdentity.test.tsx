import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AgentIcon } from "./AgentIdentity";
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
});
