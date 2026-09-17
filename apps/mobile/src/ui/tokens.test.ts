import { describe, expect, it } from "vitest";

import { colors, fixedChromeText, terminalTheme, tokens } from "./tokens";

describe("tokens", () => {
  it("uses the desktop chrome base as the terminal background", () => {
    expect(colors.chromeBg).toBe("#282c34");
    expect(terminalTheme.background).toBe(colors.chromeBg);
  });

  it("exposes every token group through the default export", () => {
    expect(Object.keys(tokens).sort()).toEqual([
      "colors",
      "fixedChromeText",
      "fonts",
      "metrics",
      "radii",
      "stateColors",
      "terminalTheme",
      "typeScale",
    ]);
  });

  it("caps only fixed-height chrome at the QA-tested font scale", () => {
    expect(fixedChromeText.maxFontSizeMultiplier).toBe(1.3);
  });
});
