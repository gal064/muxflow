import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
// Tokens first: every rule in styles.css resolves against these custom
// properties, and the terminal renderer reads them off :root at construction.
import "./tokens.css";
import "./styles.css";
import { App } from "./app/App";
import { bootstrapPerfProbe } from "./perf/bootstrap";
import { terminalFont } from "./features/terminal/theme";

void bootstrapPerfProbe();

/**
 * Longest the app will wait for its bundled font before rendering anyway.
 *
 * xterm measures one cell at construction time, and that cell size is what the
 * tmux client size is computed from. If a terminal is built before JetBrains
 * Mono is available it measures the fallback stack instead, and the app then
 * asks the user's tmux server for a grid derived from the wrong cell — the one
 * computation this project has already damaged real windows with once
 * (P12-U006). The bound is here so a font that never resolves costs a slightly
 * wrong first measure rather than an app that never starts.
 */
const FONT_READY_TIMEOUT_MS = 2_000;

/**
 * Waits for the terminal's own face, not merely for "no font load is pending".
 *
 * `document.fonts.ready` was the whole wait here, and it is not the wait it
 * reads as. A `@font-face` rule does not load its file until something on the
 * page is laid out in that family, and at this point nothing has rendered at
 * all — so there is no pending load, `ready` resolves on the spot, and the app
 * mounts into the fallback stack exactly as if the wait were not there.
 *
 * What that cost is not only a mis-measured cell. xterm rasterises each glyph
 * once into a texture atlas it never revisits, so the glyphs drawn in the first
 * frames keep the fallback face's baseline while everything rasterised after
 * the file lands gets JetBrains Mono's — one row of terminal text in two
 * typefaces, three device pixels apart. `fonts.load` starts the download and
 * resolves when the faces are usable, which is the thing worth waiting for; all
 * four styles are named because the terminal rasterises bold and italic runs
 * into the same atlas and a late bold face splits it the same way.
 */
async function fontsReady(): Promise<void> {
  const fonts = document.fonts;
  if (!fonts) return;
  const { fontFamily, fontSize } = terminalFont();
  const faces = typeof fonts.load === "function"
    ? Promise.all(["", "700 ", "italic ", "italic 700 "].map((style) => fonts.load(`${style}${fontSize}px ${fontFamily}`)))
      // `ready` still follows, so the chrome's own faces are settled too, and a
      // face this list missed is no worse off than it was before.
      .then(() => fonts.ready)
    : fonts.ready;
  await Promise.race([
    faces.then(() => undefined, () => undefined),
    new Promise<void>((resolve) => setTimeout(resolve, FONT_READY_TIMEOUT_MS)),
  ]);
}

function mount(): void {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

// Rendering is never conditional on the font check succeeding: a rejection
// between here and `render` would otherwise leave a permanently blank window
// with nothing to look at and nothing logged.
void fontsReady().then(mount, (error) => {
  console.warn("font readiness check failed; rendering anyway", error);
  mount();
});

