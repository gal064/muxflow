import { describe, expect, it } from "vitest";
// The addon's own bundle, as text. What is asserted here lives entirely inside
// the dependency — two methods we patch — and reaching it through the public
// `WebglAddon` would need a real WebGL2 context, which no test environment here
// has. So the bundle is read the way `glyphWeight.test.ts` reads it, and the
// patched method bodies are run directly against stand-ins for the objects they
// belong to. That is enough to pin the behaviour that regressed, because the
// bug was never in the WebGL calls: it was in who gets told to rebuild.
import addonBundle from "@xterm/addon-webgl/lib/addon-webgl.mjs?raw";

/**
 * One texture atlas is shared by every terminal with the same font config, and
 * merging four atlas pages into one rewrites the texture coordinates of every
 * glyph already in them. Each renderer baked the old coordinates into its own
 * vertex buffer when it last touched a cell, so a merge invalidates *every*
 * renderer's model — not just the one whose glyph request triggered it. See the
 * patch note in `pnpm-workspace.yaml`.
 */

/**
 * Lifts one method out of the minified bundle by name and rebuilds it as a
 * callable function. The bodies concerned contain no braces of their own, so
 * the first `}` closes the method.
 */
function methods(name: string): string[] {
  const opening = `${name}(){`;
  const bodies: string[] = [];
  for (let at = addonBundle.indexOf(opening); at !== -1; at = addonBundle.indexOf(opening, at + 1)) {
    const from = at + opening.length;
    const to = addonBundle.indexOf("}", from);
    expect(to).toBeGreaterThan(from);
    bodies.push(addonBundle.slice(from, to));
  }
  return bodies;
}

function method(name: string, contains: string): () => unknown {
  const matches = methods(name).filter((body) => body.includes(contains));
  // A second match would mean the bundle no longer distinguishes the two
  // `beginFrame` implementations the way this test assumes.
  expect(matches).toHaveLength(1);
  return new Function(`return function(){${matches[0]}};`)() as () => unknown;
}

/** `TextureAtlas.beginFrame` — reports how many times the atlas has been invalidated. */
const atlasBeginFrame = method("beginFrame", "_requestClearModel");
/** `GlyphRenderer.beginFrame` — answers "does *this* renderer need a full rebuild?". */
const glyphBeginFrame = method("beginFrame", "_atlasClearModelVersion");

interface FakeAtlas {
  _requestClearModel: number;
  beginFrame: () => unknown;
}

/** Stands in for a `GlyphRenderer` bound to `atlas`, freshly constructed. */
function renderer(atlas: FakeAtlas | undefined): { beginFrame: () => boolean } {
  const self = { _atlas: atlas, _atlasClearModelVersion: -1 };
  return { beginFrame: () => glyphBeginFrame.call(self) as boolean };
}

function sharedAtlas(): FakeAtlas {
  const atlas: FakeAtlas = {
    _requestClearModel: 0,
    beginFrame: () => atlasBeginFrame.call(atlas),
  };
  return atlas;
}

describe("a page merge in the shared texture atlas", () => {
  it("rebuilds the model of every renderer sharing the atlas, not just the first to ask", () => {
    const atlas = sharedAtlas();
    const [focused, sibling] = [renderer(atlas), renderer(atlas)];
    // The first frame after each renderer is bound to the atlas.
    expect(focused.beginFrame()).toBe(true);
    expect(sibling.beginFrame()).toBe(true);
    expect(focused.beginFrame()).toBe(false);
    expect(sibling.beginFrame()).toBe(false);

    // The merge. It happens while rasterising a glyph for whichever pane asked
    // for it, but it moves the glyphs both panes are drawing from.
    atlas._requestClearModel++;

    expect(focused.beginFrame()).toBe(true);
    // The regression: with a single boolean on the shared atlas, the pane that
    // did not trigger the merge kept painting from the pre-merge coordinates.
    expect(sibling.beginFrame()).toBe(true);
  });

  it("costs each renderer exactly one rebuild, however many frames follow", () => {
    const atlas = sharedAtlas();
    const pane = renderer(atlas);
    pane.beginFrame();
    atlas._requestClearModel++;
    expect(pane.beginFrame()).toBe(true);
    // Upstream never reset the flag, so every frame from here on rebuilt the
    // whole viewport for the rest of the session.
    expect([pane.beginFrame(), pane.beginFrame(), pane.beginFrame()]).toEqual([false, false, false]);
  });

  it("rebuilds a renderer that has no atlas yet, since it has nothing valid to keep", () => {
    expect(renderer(undefined).beginFrame()).toBe(true);
  });
});

describe("the atlas invalidation count", () => {
  it("is raised by every event that moves or erases a rasterised glyph", () => {
    // A page merge, the first oversized glyph (both in `_createNewPage`/
    // `_drawToCache`), and `clearTexture()`. These sites cannot be lifted out
    // of the bundle the way the two `beginFrame` bodies can, so they are pinned
    // by shape: three increments, and no assignment left behind.
    expect(addonBundle.split("this._requestClearModel++")).toHaveLength(3 + 1);
    expect(addonBundle).not.toContain("this._requestClearModel=!0");
  });

  it("is forgotten when a renderer is handed a different atlas", () => {
    // A different atlas is a different coordinate space and starts its own
    // count, so the count this renderer last acted on means nothing against it.
    // `setAtlas` takes a parameter, so it is matched by shape rather than
    // lifted out like the two `beginFrame` bodies above.
    expect(addonBundle).toMatch(/setAtlas\(\w+\)\{this\._atlas=\w+;this\._atlasClearModelVersion=-1;/u);
  });
});
