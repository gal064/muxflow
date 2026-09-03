import { describe, expect, it } from "vitest";

import { colors, terminalTheme, tokens } from "./tokens";

describe("tokens", () => {
  it("uses the desktop chrome base as the terminal background", () => {
    expect(colors.chromeBg).toBe("#282c34");
    expect(terminalTheme.background).toBe(colors.chromeBg);
  });

  it("exposes every token group through the default export", () => {
    expect(Object.keys(tokens).sort()).toEqual([
      "colors",
      "fonts",
      "metrics",
      "radii",
      "stateColors",
      "terminalTheme",
      "typeScale",
    ]);
  });
});
