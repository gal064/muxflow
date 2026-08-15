// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
// The addon's own bundle, as text. The hook below recognises the glyph atlas by
// the shape of one call inside it, and that call belongs to a dependency — so
// the assertion that it still looks like this has to read the dependency, the
// way `theme.test.ts` reads `tokens.css` for the same reason.
import addonBundle from "@xterm/addon-webgl/lib/addon-webgl.mjs?raw";
import { installAtlasFontSmoothing } from "./atlasFontSmoothing";
import { GHOSTTY_TEXT_OPTIONS } from "./theme";

/**
 * The two halves of "the terminal's glyphs weigh what the font says they
 * weigh": where they are rasterised, and which faces they are rasterised from.
 */

type ContextRequest = { canvas: HTMLCanvasElement; contextId: string; options: unknown };

const requests: ContextRequest[] = [];
const context = { marker: "context" };

beforeAll(() => {
  // Stands in for the real `getContext`, which jsdom does not implement. The
  // hook wraps whatever is on the prototype when it installs, so this has to be
  // in place first — and it lets the tests below assert that the call still
  // arrives, unaltered, at the implementation underneath.
  HTMLCanvasElement.prototype.getContext = function stub(
    this: HTMLCanvasElement,
    contextId: string,
    options?: unknown,
  ) {
    requests.push({ canvas: this, contextId, options });
    return context;
  } as unknown as HTMLCanvasElement["getContext"];
  installAtlasFontSmoothing();
  // Installing twice must not wrap twice: every pane calls this before it
  // builds its addon, and a stack of wrappers would move a canvas once per
  // terminal ever opened.
  installAtlasFontSmoothing();
});

function atlasContext(canvas: HTMLCanvasElement): unknown {
  return canvas.getContext("2d", { alpha: false, willReadFrequently: true });
}

describe("the glyph atlas canvas", () => {
  it("is in the document before the context that draws into it exists", () => {
    const canvas = document.createElement("canvas");
    expect(canvas.isConnected).toBe(false);
    expect(atlasContext(canvas)).toBe(context);
    expect(canvas.isConnected).toBe(true);
    const holder = canvas.parentElement!;
    const declared = new Map((holder.getAttribute("style") ?? "")
      .split(";")
      .map((part) => part.split(/:(.*)/s).map((half) => half.trim()))
      .filter((pair) => pair[0])
      .map(([property, value]) => [property, value] as const));
    // The declaration the whole module exists to apply, and the off-screening
    // that has to stay positional: an element that is not *rendered* is the one
    // configuration this was measured not to work in.
    expect(declared.get("-webkit-font-smoothing")).toBe("antialiased");
    expect(declared.get("left")).toBe("-9999px");
    expect(declared.has("display")).toBe(false);
    expect(declared.has("visibility")).toBe(false);
    expect(declared.has("content-visibility")).toBe(false);
    // The call reaches the real implementation with what the caller passed.
    expect(requests.at(-1)).toMatchObject({ canvas, contextId: "2d", options: { willReadFrequently: true } });
  });

  it("shares one holder with every other atlas, and joins it once", () => {
    const first = document.createElement("canvas");
    const second = document.createElement("canvas");
    atlasContext(first);
    atlasContext(second);
    const holder = first.parentElement!;
    expect(second.parentElement).toBe(holder);
    // Asking again for a canvas that is already placed leaves it where it is,
    // rather than moving it to the end or adding a second copy.
    const before = holder.querySelectorAll("canvas").length;
    atlasContext(first);
    expect(first.parentElement).toBe(holder);
    expect(holder.querySelectorAll("canvas")).toHaveLength(before);
    expect(holder.lastElementChild).toBe(second);
  });

  it("leaves every canvas that is not a glyph atlas where it was", () => {
    const cases: Array<[string, unknown]> = [
      // The addon's atlas *pages*: they only ever receive blits.
      ["2d", { alpha: true }],
      ["2d", undefined],
      // An ordinary offscreen measuring canvas — the same `willReadFrequently`
      // idiom without the addon's `alpha`. Nothing ever removes a canvas from
      // the holder, so claiming a stranger's would pin it in the document for
      // the life of the app.
      ["2d", { willReadFrequently: true }],
      ["webgl2", { antialias: false }],
    ];
    for (const [contextId, options] of cases) {
      const canvas = document.createElement("canvas");
      canvas.getContext(contextId as "2d", options as CanvasRenderingContext2DSettings);
      expect(canvas.isConnected, `${contextId} ${JSON.stringify(options)}`).toBe(false);
    }
    // A canvas the app has already put on screen is not relocated under it.
    const onScreen = document.createElement("canvas");
    document.body.appendChild(onScreen);
    atlasContext(onScreen);
    expect(onScreen.parentElement).toBe(document.body);
  });

  it("is still asking for its context the way the hook recognises", () => {
    // The hook fails open: an addon that stops passing these two options
    // together stops being recognised, and the only symptom is glyphs that go
    // quietly back to being heavier than the rest of the app. This is the
    // tripwire for that — it reads the dependency rather than trusting the
    // signature re-typed in `atlasContext` above, which would keep passing.
    expect(addonBundle, "the glyph atlas no longer asks for alpha + willReadFrequently")
      .toMatch(/getContext\(\s*["']2d["']\s*,\s*\{[^}]*\balpha\s*:[^}]*\bwillReadFrequently\s*:\s*!?(0|1|true)/);
    // And the idiom the hook must keep refusing is still in the same bundle,
    // which is what makes the `alpha` half of the predicate load-bearing rather
    // than decorative. If only this half ever fails, nothing is broken — the
    // narrowing has simply stopped being necessary here.
    expect(addonBundle, "the willReadFrequently-alone idiom this predicate excludes is gone")
      .toMatch(/getContext\(\s*["']2d["']\s*,\s*\{\s*willReadFrequently\s*:\s*!?(0|1|true)\s*\}/);
  });
});

describe("the weights the terminal is allowed to draw", () => {
  it("are the ones xterm still has options for", () => {
    // A tripwire on the xterm upgrade, not on the values: an option xterm
    // renames or drops is silently ignored, and the symptom is bold text that
    // is a shade brighter than the terminal this app is matching.
    const terminal = new Terminal(GHOSTTY_TEXT_OPTIONS);
    try {
      expect(terminal.options.drawBoldTextInBrightColors).toBe(false);
      expect(terminal.options.fontWeight).toBe("normal");
      expect(terminal.options.fontWeightBold).toBe("bold");
    } finally {
      terminal.dispose();
    }
  });

  it("leaves contrast alone, because Ghostty does", () => {
    // Ghostty applies no contrast adjustment, and xterm's default is likewise
    // 1 (off). Named here so raising it stays a decision rather than a drift.
    const terminal = new Terminal(GHOSTTY_TEXT_OPTIONS);
    try {
      expect(terminal.options.minimumContrastRatio).toBe(1);
    } finally {
      terminal.dispose();
    }
  });
});
