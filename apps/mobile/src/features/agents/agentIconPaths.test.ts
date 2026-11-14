import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { CLAUDE_ICON, CODEX_ICON, GENERIC_ICON, isClaudeAdapter, isCodexAdapter, isPiAdapter, PI_ICON, PIN_ICON } from "./agentIconPaths";

const view = readFileSync(resolve(__dirname, "../../../../desktop/src/agents/view.rs"), "utf8");
const icons = readFileSync(resolve(__dirname, "../../../../desktop/src/ui/icons.rs"), "utf8");

/** The SVG source of one of the desktop's `const NAME: &[u8] = br##"<svg …>"##;` icons. */
function svg(name: string): string {
  const match = new RegExp(`const ${name}: &\\[u8\\] = br##"(<svg[^]*?</svg>)"##;`).exec(view);
  if (!match) throw new Error(`no ${name} icon in agents/view.rs`);
  return match[1]!;
}

/** Every `attr="..."` value in the source, in source order. */
function attrs(source: string, name: string): string[] {
  return [...source.matchAll(new RegExp(`\\b${name}="([^"]*)"`, "g"))].map((m) => m[1]!);
}

describe("agent icon geometry matches the desktop's agents/view.rs byte-for-byte", () => {
  it("Claude: the four-stroke mark, its viewBox and stroke width", () => {
    const claude = svg("CLAUDE");
    expect(attrs(claude, "d")).toEqual([CLAUDE_ICON.path]);
    expect(attrs(claude, "viewBox")).toEqual([CLAUDE_ICON.viewBox]);
    expect(attrs(claude, "stroke-width")).toEqual([String(CLAUDE_ICON.strokeWidth)]);
  });

  it("Codex: the OpenAI blossom path, its viewBox and the optical-inset transform", () => {
    const codex = svg("CODEX");
    expect(attrs(codex, "d")).toEqual([CODEX_ICON.path]);
    expect(attrs(codex, "viewBox")).toEqual([CODEX_ICON.viewBox]);
    expect(attrs(codex, "transform")).toEqual([CODEX_ICON.transform]);
  });

  it("pi: pi.dev's mark, its viewBox and its inset", () => {
    const pi = svg("PI");
    expect(attrs(pi, "d")).toEqual([PI_ICON.path]);
    expect(attrs(pi, "viewBox")).toEqual([PI_ICON.viewBox]);
    expect(attrs(pi, "transform")).toEqual([PI_ICON.transform]);
  });

  it("generic: the face", () => {
    const generic = svg("GENERIC");
    expect(attrs(generic, "d")).toEqual([GENERIC_ICON.mouth]);
    expect(attrs(generic, "viewBox")).toEqual([GENERIC_ICON.viewBox]);
    const circles = [...generic.matchAll(/<circle cx="([^"]*)" cy="([^"]*)" r="([^"]*)"/g)].map((m) => ({ cx: Number(m[1]), cy: Number(m[2]), r: Number(m[3]) }));
    expect(circles).toEqual([GENERIC_ICON.face, ...GENERIC_ICON.eyes]);
    expect(attrs(generic, "stroke-width")).toEqual([String(GENERIC_ICON.strokeWidth)]);
  });

  it("pin: the desktop's shared pin glyph (ui/icons.rs), its box and stroke", () => {
    const body = /Self::Pin => glyph!\(\s*r#"([^]*?)"#\s*\)/.exec(icons)?.[1];
    if (!body) throw new Error("no Pin glyph in ui/icons.rs");
    const frame = /macro_rules! glyph[^]*?r##"(<svg[^]*?>)"##/.exec(icons)?.[1];
    if (!frame) throw new Error("no glyph! frame in ui/icons.rs");
    expect(attrs(body, "d")).toEqual([...PIN_ICON.paths]);
    expect(attrs(frame, "viewBox")).toEqual([PIN_ICON.viewBox]);
    expect(attrs(frame, "stroke-width")).toEqual([String(PIN_ICON.strokeWidth)]);
  });

  it("adapter ids resolve the way the desktop's adapter_icon branches do", () => {
    expect(view).toContain('"claude-code" => CLAUDE');
    expect(view).toContain('"codex" => CODEX');
    expect(view).toContain('"pi" => PI');
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
