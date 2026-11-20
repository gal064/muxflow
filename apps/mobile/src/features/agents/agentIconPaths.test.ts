import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { CLAUDE_ICON, CODEX_ICON, GENERIC_ICON, isClaudeAdapter, isCodexAdapter, isPiAdapter, PI_ICON, PIN_ICON } from "./agentIconPaths";

const desktopSrc = resolve(__dirname, "../../../../desktop-gpui/src");
const view = readFileSync(resolve(desktopSrc, "agents/view.rs"), "utf8");
const icons = readFileSync(resolve(desktopSrc, "ui/icons.rs"), "utf8");

/** The SVG in `const NAME: &[u8] = br##"…"##;`. */
function mark(name: string): string {
  const svg = view.match(new RegExp(`^const ${name}: &\\[u8\\] = br##"(.*)"##;$`, "m"))?.[1];
  if (svg === undefined) throw new Error(`${name} is not in agents/view.rs`);
  return svg;
}

/** Every `attr="..."` value in the source, in order. */
function attrs(source: string, name: string): string[] {
  return [...source.matchAll(new RegExp(`\\b${name}="([^"]*)"`, "g"))].map((m) => m[1]!);
}

describe("agent icon geometry matches the desktop's agents/view.rs byte-for-byte", () => {
  it("Claude: the four-stroke mark, its viewBox and stroke width", () => {
    const svg = mark("CLAUDE");
    expect(attrs(svg, "d")).toEqual([CLAUDE_ICON.path]);
    expect(attrs(svg, "viewBox")).toEqual([CLAUDE_ICON.viewBox]);
    expect(attrs(svg, "stroke-width")).toEqual([String(CLAUDE_ICON.strokeWidth)]);
  });

  it("Codex: the OpenAI blossom path, its viewBox and the optical-inset transform", () => {
    const svg = mark("CODEX");
    expect(attrs(svg, "d")).toEqual([CODEX_ICON.path]);
    expect(attrs(svg, "viewBox")).toEqual([CODEX_ICON.viewBox]);
    expect(attrs(svg, "transform")).toEqual([CODEX_ICON.transform]);
  });

  it("pi: pi.dev's mark, its viewBox and its inset", () => {
    const svg = mark("PI");
    expect(attrs(svg, "d")).toEqual([PI_ICON.path]);
    expect(attrs(svg, "viewBox")).toEqual([PI_ICON.viewBox]);
    expect(attrs(svg, "transform")).toEqual([PI_ICON.transform]);
  });

  it("generic: the face", () => {
    const svg = mark("GENERIC");
    expect(attrs(svg, "d")).toEqual([GENERIC_ICON.mouth]);
    expect(attrs(svg, "viewBox")).toEqual([GENERIC_ICON.viewBox]);
    const circles = [...svg.matchAll(/<circle cx="([^"]*)" cy="([^"]*)" r="([^"]*)"/g)].map((m) => ({ cx: Number(m[1]), cy: Number(m[2]), r: Number(m[3]) }));
    expect(circles).toEqual([GENERIC_ICON.face, ...GENERIC_ICON.eyes]);
    expect(attrs(svg, "stroke-width")).toEqual([String(GENERIC_ICON.strokeWidth)]);
  });

  it("pin: the desktop glyph set's pin, at its stroke width", () => {
    const start = icons.indexOf("Self::Pin => glyph!(");
    const pin = icons.slice(start, icons.indexOf("),", start));
    expect(attrs(pin, "d")).toEqual([...PIN_ICON.paths]);
    expect(icons).toContain(`viewBox="${PIN_ICON.viewBox}" fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round" stroke-width="${PIN_ICON.strokeWidth}"`);
  });

  it("adapter ids resolve the way the desktop's mark lookup does", () => {
    expect(view).toContain('"claude-code" => CLAUDE,');
    expect(view).toContain('"codex" => CODEX,');
    expect(view).toContain('"pi" => PI,');
    expect(isClaudeAdapter("claude-code")).toBe(true);
    expect(isClaudeAdapter("claude")).toBe(false);
    expect(isClaudeAdapter("codex")).toBe(false);
    expect(isCodexAdapter("codex")).toBe(true);
    expect(isCodexAdapter("claude-code")).toBe(false);
    expect(isCodexAdapter("other")).toBe(false);
    expect(isPiAdapter("pi")).toBe(true);
    expect(isPiAdapter("pip")).toBe(false);
  });
});
