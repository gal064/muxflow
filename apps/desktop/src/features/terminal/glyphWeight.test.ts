// @vitest-environment jsdom
import { beforeAll, describe, expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
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
    const style = holder.getAttribute("style") ?? "";
    // The declaration the whole module exists to apply.
    expect(style).toContain("-webkit-font-smoothing: antialiased");
    // Off-screen, not undisplayed: an element that is not rendered is the one
    // configuration this was measured *not* to work in.
    expect(style).toContain("left: -9999px");
    expect(style).not.toContain("display");
    expect(style).not.toContain("visibility");
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
