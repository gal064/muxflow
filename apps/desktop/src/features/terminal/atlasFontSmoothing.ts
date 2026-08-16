/**
 * Why the terminal's glyphs are heavier than the same font everywhere else, and
 * the one line of DOM that fixes it.
 *
 * The app and Ghostty render JetBrains Mono Regular at 13px from the same file,
 * and the app's looked semi-bold beside it. The font is not the cause: all four
 * faces are bundled and the app refuses to mount until they load
 * (`main.tsx`), so a fallback stack is not reachable. The cause is where the
 * glyphs are rasterised. xterm's WebGL addon builds a glyph atlas by calling
 * `fillText` on a canvas it creates with `document.createElement` and never
 * attaches, and WebKit resolves `-webkit-font-smoothing` for canvas text from
 * the canvas **element's computed style** — which a detached element does not
 * have. So `:root { -webkit-font-smoothing: antialiased }` in `styles.css`
 * reaches every glyph in the app except the ones in the terminal, and those get
 * CoreGraphics' default subpixel smoothing with stem darkening baked in.
 *
 * Measured in Playwright WebKit against the addon's exact atlas path
 * (`getContext("2d", { alpha, willReadFrequently: true })`, background fill,
 * `fillText`), as ink mass — summed luminance above the background — over the
 * text band, with a DOM line of the same text at `-webkit-font-smoothing:
 * antialiased` as the reference:
 *
 * | case                                                  | ink mass | vs reference |
 * | ----------------------------------------------------- | -------- | ------------ |
 * | DOM text, antialiased (the rest of the app)            |   504.8k | reference    |
 * | detached canvas (the addon today)                      |   665.5k | +32%         |
 * | detached canvas, `alpha: true`                         |   665.5k | +32%         |
 * | detached canvas + inline `-webkit-font-smoothing`      |   665.5k | +32%         |
 * | canvas attached to the document, smoothing inherited   |   504.8k | exact        |
 *
 * Two things that row 2 and row 3 rule out, so they are not tried again:
 * xterm's `allowTransparency` (which is what feeds `alpha` here) changes
 * nothing, and neither does styling the canvas while it is detached. Only being
 * in the document does.
 *
 * Hence this module: a hook on `getContext` that puts the atlas canvas into a
 * hidden corner of the document before the addon draws into it. It has to be a
 * standing hook rather than a step in terminal setup, because the addon builds
 * new atlas canvases long after activation — whenever the font, the theme or
 * the device pixel ratio changes, and on every `clearTextureAtlas`.
 */

/**
 * Recognises the atlas canvas, and deliberately little else.
 *
 * The addon asks for its glyph canvas as `{ alpha, willReadFrequently: true }`
 * (`TextureAtlas._tmpCanvas`), and that pair is the whole predicate. Both
 * halves are load-bearing in the *narrowing* direction, because this hook is a
 * global patch and everything it claims it also keeps: `willReadFrequently`
 * alone is the ordinary idiom for an offscreen measuring canvas — xterm's own
 * colour parser uses it — and those must be left where they are, since nothing
 * here would ever take them back out of the document. Requiring `alpha` to have
 * been named is what separates the addon's call from that idiom.
 *
 * The atlas *pages* pass neither `willReadFrequently` nor text: they only
 * receive `drawImage` blits of this canvas, so they need no hook and match
 * none. Requiring the canvas to be detached keeps the hook off everything the
 * app itself puts on screen, and makes it idempotent — a canvas already moved
 * here is connected, and does not match a second time.
 *
 * If a future xterm stops passing `alpha`, this stops matching and the glyphs
 * go back to being heavy. That is the failure this trades for, and it is the
 * one that can be made loud: `glyphWeight.test.ts` reads the bundled addon and
 * fails if the signature this keys on is no longer in it. The failure it trades
 * away — a stranger's canvas pinned into the document forever — has no such
 * tripwire, because nothing would look wrong.
 */
function wantsDocumentFontSmoothing(
  canvas: HTMLCanvasElement,
  contextId: string,
  options: unknown,
): boolean {
  if (contextId !== "2d" || canvas.isConnected) return false;
  if (typeof options !== "object" || options === null) return false;
  return (options as CanvasRenderingContext2DSettings).willReadFrequently === true
    && "alpha" in options;
}

const HOLDER_ID = "ade-atlas-font-smoothing";

/**
 * Written as one declaration string rather than through `style.setProperty`,
 * because `-webkit-font-smoothing` — the only declaration here that does any
 * work — is a prefixed property that engines outside WebKit, the test runner's
 * included, drop on the way in.
 */
const HOLDER_STYLE = "position: absolute; left: -9999px; top: 0; -webkit-font-smoothing: antialiased";

let holder: HTMLElement | undefined;

/**
 * The off-screen element the atlas canvases live under.
 *
 * Off-screen by position, deliberately not by `display: none` or
 * `visibility: hidden`: an element in either of those states is still in the
 * document, but this is the configuration the measurement above was taken in
 * and the difference is not worth re-deriving to save nothing.
 *
 * The lifetime note, because this hook changes one: an atlas canvas arrives
 * here reachable only from the addon and leaves rooted in the document, so it
 * is no longer the garbage collector that ends it. `TextureAtlas.dispose()`
 * calls `remove()` on the canvas it owns, which unroots it again — a fact about
 * a dependency that this module now depends on, and the reason nothing here
 * sweeps the holder.
 */
function fontSmoothingHolder(): HTMLElement | undefined {
  if (holder?.isConnected) return holder;
  const parent = globalThis.document?.body ?? globalThis.document?.documentElement;
  if (!parent) return undefined;
  const element = globalThis.document.createElement("div");
  element.id = HOLDER_ID;
  element.setAttribute("aria-hidden", "true");
  // Inherited by everything under it, which is the whole mechanism.
  element.setAttribute("style", HOLDER_STYLE);
  parent.appendChild(element);
  holder = element;
  return element;
}

type GetContext = HTMLCanvasElement["getContext"];

let installed = false;

/**
 * Installs the hook. Idempotent, and safe to call from anywhere that is about
 * to build a terminal; there is no matching uninstall because the hook has no
 * effect on a canvas that does not look like a glyph atlas.
 *
 * A call rather than an import side effect, even though the module has exactly
 * one global effect and importing it for that effect would be shorter. The
 * ordering is the whole point — the hook has to be in place before an atlas is
 * built, and a bare `import "./atlasFontSmoothing"` states that ordering in the
 * one place a bundler is free to rearrange and a linter is free to drop.
 */
export function installAtlasFontSmoothing(): void {
  if (installed) return;
  const prototype = globalThis.HTMLCanvasElement?.prototype;
  if (!prototype) return;
  installed = true;
  const original = prototype.getContext as (
    this: HTMLCanvasElement,
    contextId: string,
    options?: unknown,
  ) => RenderingContext | null;
  prototype.getContext = function patched(
    this: HTMLCanvasElement,
    contextId: string,
    options?: unknown,
  ): RenderingContext | null {
    if (wantsDocumentFontSmoothing(this, contextId, options)) {
      // Before the context exists, so the first glyph is rasterised under the
      // same computed style as the ten thousandth.
      fontSmoothingHolder()?.appendChild(this);
    }
    return original.call(this, contextId, options);
  } as GetContext;
}
