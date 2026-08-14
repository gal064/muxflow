import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
// Tokens first: every rule in styles.css resolves against these custom
// properties, and the terminal renderer reads them off :root at construction.
import "./tokens.css";
import "./styles.css";
import { App } from "./app/App";
import { bootstrapPerfProbe } from "./perf/bootstrap";

void bootstrapPerfProbe();

/**
 * Longest the app will wait for its bundled font before rendering anyway.
 *
 * xterm measures one cell at construction time, and that cell size is what the
 * tmux client size is computed from. If a terminal is built before JetBrains
 * Mono is available it measures the fallback stack instead, and the app then
 * asks the user's tmux server for a grid derived from the wrong cell — the one
 * computation this project has already damaged real windows with once
 * (P12-U006). Waiting for `document.fonts.ready` removes that race; the bound
 * is here so a font that never resolves costs a slightly wrong first measure
 * rather than an app that never starts.
 */
const FONT_READY_TIMEOUT_MS = 2_000;

async function fontsReady(): Promise<void> {
  const fonts = document.fonts;
  if (!fonts) return;
  await Promise.race([
    fonts.ready.then(() => undefined),
    new Promise<void>((resolve) => setTimeout(resolve, FONT_READY_TIMEOUT_MS)),
  ]);
}

void fontsReady().then(() => {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
});

