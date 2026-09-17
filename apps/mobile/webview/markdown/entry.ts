// The Markdown WebView page (design doc §10.3).
//
// This file is bundled by `scripts/build-webview.mjs` (esbuild, IIFE) together
// with `marked` and `DOMPurify`, inlined into `index.html`, and emitted as a
// TypeScript module the app imports. It never loads anything over the network:
// the WebView is handed the whole document as a string.
//
// Message contract, JSON over `postMessage`:
//
//   RN → page   { t: "render", source }   render Markdown into the body
//   page → RN   { t: "ready" }            the page is listening
//   page → RN   { t: "link", href }       a link was tapped; RN decides

import { renderSafeMarkdown } from "../../src/features/files/markdown";

declare global {
  interface Window {
    ReactNativeWebView?: { postMessage(message: string): void };
  }
}

type Outbound = { t: "ready" } | { t: "link"; href: string };

function post(message: Outbound): void {
  window.ReactNativeWebView?.postMessage(JSON.stringify(message));
}

function render(source: string): void {
  const article = document.getElementById("content");
  if (!article) return;
  article.innerHTML = renderSafeMarkdown(source);
  window.scrollTo(0, 0);
}

function handle(data: unknown): void {
  if (typeof data !== "string") return;
  let message: { t?: unknown; source?: unknown };
  try {
    message = JSON.parse(data) as { t?: unknown; source?: unknown };
  } catch {
    return;
  }
  if (message.t === "render" && typeof message.source === "string") render(message.source);
}

// react-native-webview delivers `postMessage` on `document` on Android and on
// `window` on iOS; listening on both is the documented way to be portable.
document.addEventListener("message", (event) => handle((event as MessageEvent).data));
window.addEventListener("message", (event) => handle(event.data));

// Every link is decided by RN (§10.3): http(s) and mailto open in the system
// browser, everything else — including relative links into the repository —
// does nothing.
document.addEventListener("click", (event) => {
  const target = event.target;
  const anchor = target instanceof Element ? target.closest("a[href]") : null;
  if (!anchor) return;
  event.preventDefault();
  post({ t: "link", href: anchor.getAttribute("href") ?? "" });
});

post({ t: "ready" });
