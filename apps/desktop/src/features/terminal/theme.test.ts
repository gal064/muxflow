import { describe, expect, it } from "vitest";
// The token file itself, as text: this test's whole purpose is to compare the
// renderer's fallback against what tokens.css actually declares.
import tokensCss from "../../tokens.css?raw";
import { CHROME_FALLBACKS, GHOSTTY_DEFAULT_DARK, searchDecorations, terminalFont, terminalTheme } from "./theme";

function token(name: string): string | undefined {
  return new RegExp(`^\\s*${name}:\\s*([^;]+);`, "mu").exec(tokensCss)?.[1].trim();
}

describe("terminal theme derivation", () => {
  it("keeps the renderer fallback identical to the token file", () => {
    // The fallback exists only for environments with no stylesheet. If it ever
    // disagrees with tokens.css the app has two palettes again, which is the
    // exact defect Phase 11 removed.
    const expected: Record<string, string | undefined> = {
      background: token("--term-bg"),
      foreground: token("--term-fg"),
      cursor: token("--term-cursor"),
      cursorAccent: token("--term-cursor-text"),
      selectionBackground: token("--term-selection-bg"),
      selectionForeground: token("--term-selection-fg"),
      black: token("--term-0"),
      red: token("--term-1"),
      green: token("--term-2"),
      yellow: token("--term-3"),
      blue: token("--term-4"),
      magenta: token("--term-5"),
      cyan: token("--term-6"),
      white: token("--term-7"),
      brightBlack: token("--term-8"),
      brightRed: token("--term-9"),
      brightGreen: token("--term-10"),
      brightYellow: token("--term-11"),
      brightBlue: token("--term-12"),
      brightMagenta: token("--term-13"),
      brightCyan: token("--term-14"),
      brightWhite: token("--term-15"),
    };
    for (const [key, value] of Object.entries(expected)) {
      expect(value, `tokens.css is missing the token behind ${key}`).toBeDefined();
      expect(GHOSTTY_DEFAULT_DARK[key as keyof typeof GHOSTTY_DEFAULT_DARK], key).toBe(value);
    }
  });

  it("is Ghostty Default Style Dark, so the terminal matches the user's own terminal", () => {
    // Verbatim from the theme file shipped with Ghostty on this machine.
    expect(GHOSTTY_DEFAULT_DARK.background).toBe("#282c34");
    expect(GHOSTTY_DEFAULT_DARK.foreground).toBe("#ffffff");
    expect(GHOSTTY_DEFAULT_DARK.cursor).toBe("#ffffff");
    // Ghostty inverts the selection: white ground, terminal-background ink.
    expect(GHOSTTY_DEFAULT_DARK.selectionBackground).toBe("#ffffff");
    expect(GHOSTTY_DEFAULT_DARK.selectionForeground).toBe("#282c34");
  });

  it("falls back cleanly with no stylesheet, and derives the scrollbar from chrome ink", () => {
    const theme = terminalTheme(undefined);
    expect(theme.background).toBe(GHOSTTY_DEFAULT_DARK.background);
    expect(theme.scrollbarSliderBackground).toBe("#7d848e40");
    expect(theme.scrollbarSliderHoverBackground).toBe("#7d848e66");
  });

  it("takes the terminal font and metrics from tokens, not from a literal", () => {
    expect(token("--term-font-size")).toBe("13px");
    expect(token("--term-line-height")).toBe("1.42");
    expect(token("--font-mono")).toContain("JetBrains Mono");
    const font = terminalFont(undefined);
    expect(font.fontSize).toBe(13);
    expect(font.lineHeight).toBe(1.42);
    expect(font.fontFamily).toContain("JetBrains Mono");
  });

  it("guards the chrome fallbacks too, not just the terminal palette", () => {
    // The scrollbar sliders and the search decorations are drawn by xterm from
    // literal values, so they are the one place chrome tokens can drift without
    // anything on screen changing colour in a way a person would notice.
    for (const [name, value] of Object.entries(CHROME_FALLBACKS)) {
      expect(token(name), `tokens.css is missing ${name}`).toBeDefined();
      expect(token(name), name).toBe(value);
    }
    const decorations = searchDecorations(undefined);
    expect(decorations.activeMatchColorOverviewRuler).toBe(token("--accent"));
    expect(decorations.matchOverviewRuler).toBe(token("--chrome-dim"));
  });
});
