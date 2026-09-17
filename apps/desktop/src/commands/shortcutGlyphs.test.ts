import { describe, expect, it } from "vitest";
import { commandRegistry, shortcutFor } from "./registry";
import { shortcutGlyphs, shortcutSpoken } from "./shortcutGlyphs";

describe("platform shortcut rendering", () => {
  it("renders macOS shortcuts as unseparated glyphs in the mock's modifier order", () => {
    // Every one of these appears in docs/history/plan/phase11-ui-mock.html, which fixes the
    // order at ⌃⌥⌘⇧ — Command before Shift.
    expect(shortcutGlyphs("Meta+Shift+P", "mac")).toBe("⌘⇧P");
    expect(shortcutGlyphs("Meta+K", "mac")).toBe("⌘K");
    expect(shortcutGlyphs("Ctrl+Meta+Shift+=", "mac")).toBe("⌃⌘⇧=");
    expect(shortcutGlyphs("Meta+Shift+Enter", "mac")).toBe("⌘⇧↩");
    expect(shortcutGlyphs("Meta+Alt+B", "mac")).toBe("⌥⌘B");
    expect(shortcutGlyphs("Meta+Alt+ArrowLeft", "mac")).toBe("⌥⌘←");
  });

  it("renders Linux shortcuts as words, which is that platform's own notation", () => {
    expect(shortcutGlyphs("Ctrl+Shift+P", "linux")).toBe("Ctrl+Shift+P");
    expect(shortcutGlyphs("Alt+ArrowLeft", "linux")).toBe("Alt+Left");
    expect(shortcutGlyphs("Meta+B", "linux")).toBe("Super+B");
  });

  it("renders nothing for an unbound command rather than an empty lozenge", () => {
    expect(shortcutGlyphs(undefined, "mac")).toBeUndefined();
    expect(shortcutGlyphs("", "mac")).toBeUndefined();
    expect(shortcutGlyphs("Meta+", "mac")).toBe("⌘");
  });

  it("speaks what it draws, because the glyphs announce as nothing", () => {
    expect(shortcutSpoken("Meta+Shift+P", "mac")).toBe("Command Shift P");
    expect(shortcutSpoken("Meta+Alt+ArrowLeft", "mac")).toBe("Option Command Arrow Left");
    expect(shortcutSpoken("Ctrl+Meta+Shift+=", "mac")).toBe("Control Command Shift =");
    expect(shortcutSpoken("Ctrl+Shift+P", "linux")).toBe("Control Shift P");
    expect(shortcutSpoken(undefined, "mac")).toBeUndefined();
  });

  it("leaves no registry default rendering as a raw stored binding (M10-E052)", () => {
    for (const platform of ["mac", "linux"] as const) {
      for (const command of commandRegistry) {
        const rendered = shortcutGlyphs(shortcutFor(command, platform, {}), platform);
        if (rendered === undefined) continue;
        expect(rendered, `${command.id} on ${platform}`).not.toContain("Meta");
        if (platform === "mac") expect(rendered, `${command.id} on mac`).not.toContain("+");
      }
    }
  });
});
