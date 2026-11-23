import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { lightTerminalTheme, terminalTheme } from "@muxflow/client-core";

import { schemeForBackground, terminalThemeFor } from "./terminalScheme";
import { terminalDocument } from "./terminalDocument";

describe("terminal scheme", () => {
  it("follows the brightness of the background the host reports", () => {
    expect(schemeForBackground(lightTerminalTheme.background)).toBe("light");
    expect(schemeForBackground(terminalTheme.background)).toBe("dark");
    expect(schemeForBackground("#FAFAFA")).toBe("light");
    expect(schemeForBackground("#000000")).toBe("dark");
  });

  it("stays dark for a background it cannot read", () => {
    for (const bad of ["", "fcfcfc", "#fcfcf", "#fcfcfcf", "#gggggg"]) expect(schemeForBackground(bad)).toBe("dark");
  });

  it("draws each scheme in its shared palette", () => {
    expect(terminalThemeFor("light")).toBe(lightTerminalTheme);
    expect(terminalThemeFor("dark")).toBe(terminalTheme);
  });

  it("gives the page the terminal's own background in each scheme", () => {
    // The page shows through the gutter left of the terminal, so a drift from
    // the palette would be a visible strip.
    const page = readFileSync(new URL("../../../webview/terminal/index.html", import.meta.url), "utf8");
    const backgrounds = [...page.matchAll(/--page-bg: (#[0-9a-f]{6});/gi)].map((match) => match[1]);
    expect(backgrounds).toEqual([terminalTheme.background, lightTerminalTheme.background]);
  });

  it("puts the scheme on the page before its first paint", () => {
    expect(terminalDocument("light")).toContain('<html lang="en" data-scheme="light">');
  });
});
