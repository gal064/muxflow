import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { CLAUDE_ICON, CODEX_ICON, GENERIC_ICON, isClaudeAdapter, isCodexAdapter, PIN_ICON } from "./agentIconPaths";

const desktopSrc = resolve(__dirname, "../../../../desktop/src");
const identity = readFileSync(resolve(desktopSrc, "features/agents/AgentIdentity.tsx"), "utf8");
const icon = readFileSync(resolve(desktopSrc, "ui/Icon.tsx"), "utf8");
const css = readFileSync(resolve(desktopSrc, "styles.css"), "utf8");

/** Every `attr="..."` value in the file, in source order. */
function attrs(source: string, name: string): string[] {
  return [...source.matchAll(new RegExp(`\\b${name}="([^"]*)"`, "g"))].map((m) => m[1]!);
}

describe("agent icon geometry matches the desktop's AgentIdentity.tsx byte-for-byte", () => {
  const paths = attrs(identity, "d");
  const viewBoxes = attrs(identity, "viewBox");

  it("Claude: the four-stroke mark and its viewBox", () => {
    expect(paths[0]).toBe(CLAUDE_ICON.path);
    expect(viewBoxes[0]).toBe(CLAUDE_ICON.viewBox);
    expect(css).toContain(".agent-icon.claude { stroke-width: 2; }");
    expect(CLAUDE_ICON.strokeWidth).toBe(2);
  });

  it("Codex: the OpenAI blossom path, its viewBox and the optical-inset transform", () => {
    expect(paths[1]).toBe(CODEX_ICON.path);
    expect(viewBoxes[1]).toBe(CODEX_ICON.viewBox);
    expect(attrs(identity, "transform")).toEqual([CODEX_ICON.transform]);
    expect(identity).toContain('data-icon-source="openai-blossom"');
  });

  it("generic: the face", () => {
    expect(paths[2]).toBe(GENERIC_ICON.mouth);
    expect(viewBoxes[2]).toBe(GENERIC_ICON.viewBox);
    const circles = [...identity.matchAll(/<circle cx="([^"]*)" cy="([^"]*)" r="([^"]*)" \/>/g)].map((m) => ({ cx: Number(m[1]), cy: Number(m[2]), r: Number(m[3]) }));
    expect(circles).toEqual([GENERIC_ICON.face, ...GENERIC_ICON.eyes]);
    expect(css).toMatch(/\.agent-icon\.claude, \.agent-icon\.generic \{[^}]*stroke-width: 1\.35/);
    expect(GENERIC_ICON.strokeWidth).toBe(1.35);
  });

  it("the desktop file has exactly these three icons", () => {
    expect(paths).toHaveLength(3);
    expect(viewBoxes).toHaveLength(3);
  });

  it("pin: the desktop Icon set's pin, at its stroke width", () => {
    const pin = icon.slice(icon.indexOf("  pin: <>"), icon.indexOf("</>", icon.indexOf("  pin: <>")));
    expect(attrs(pin, "d")).toEqual([...PIN_ICON.paths]);
    expect(icon).toContain("strokeWidth={1.4}");
    expect(PIN_ICON.strokeWidth).toBe(1.4);
    expect(icon).toContain('viewBox="0 0 16 16"');
  });

  it("adapter ids resolve the way the desktop's AgentIcon branches do", () => {
    expect(identity).toContain('adapterId === "claude-code" || adapterId === "claude"');
    expect(identity).toContain('adapterId === "codex"');
    expect(isClaudeAdapter("claude-code")).toBe(true);
    expect(isClaudeAdapter("claude")).toBe(true);
    expect(isClaudeAdapter("codex")).toBe(false);
    expect(isCodexAdapter("codex")).toBe(true);
    expect(isCodexAdapter("claude-code")).toBe(false);
    expect(isCodexAdapter("other")).toBe(false);
  });
});
