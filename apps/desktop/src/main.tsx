import { StrictMode, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
// Tokens first: every rule in styles.css resolves against these custom
// properties, and the terminal renderer reads them off :root at construction.
import "./tokens.css";
import "./styles.css";
import { App } from "./app/App";
import { bootstrapPerfProbe } from "./perf/bootstrap";
import { waitForTerminalFonts } from "./startup/fontGate";
import { recordPerfMilestone } from "./perf/probe";

const moduleStartedAt = performance.now();
const perfReady = bootstrapPerfProbe().then((enabled) => {
  if (enabled) recordPerfMilestone("startup.moduleStart", moduleStartedAt);
  return enabled;
});
let firstReactCommitCaptured = false;

function StartupCommitMilestone() {
  useLayoutEffect(() => {
    if (firstReactCommitCaptured) return;
    firstReactCommitCaptured = true;
    const atMs = performance.now();
    void perfReady.then((enabled) => {
      if (enabled) recordPerfMilestone("startup.firstReactCommit", atMs);
    });
  }, []);
  return null;
}

function mount(): void {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <StartupCommitMilestone />
      <App />
    </StrictMode>,
  );
}

// Rendering is never conditional on the font check succeeding: a rejection
// between here and `render` would otherwise leave a permanently blank window
// with nothing to look at and nothing logged.
void waitForTerminalFonts().then((outcome) => {
  const atMs = performance.now();
  void perfReady.then((enabled) => {
    if (enabled) recordPerfMilestone(`startup.font.${outcome}`, atMs);
  });
  mount();
}, (error) => {
  console.warn("font readiness check failed; rendering anyway", error);
  const atMs = performance.now();
  void perfReady.then((enabled) => {
    if (enabled) recordPerfMilestone("startup.font.error", atMs);
  });
  mount();
});
